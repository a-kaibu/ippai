//! Media Foundation: MFStartup, IMFAttributes (MFCreateAttributes), the DXGI device manager and
//! the Media Engine (CLSID_MFMediaEngineClassFactory) in frame-server mode, as AdvHD (x64) plays
//! its movies: SetSource, the events its IMFMediaEngineNotify gets, OnVideoStreamTick and
//! TransferVideoFrame into the game's D3D11 texture.
//!
//! A movie with a `.vnv` next to it (`masu pack`) plays through dshow_play's movie thread
//! (its clock and sound; no window of its own: the game draws the frames it takes); any other
//! source fails to load (MF_MEDIA_ENGINE_ERR_SRC_NOT_SUPPORTED), so the game goes on past it.
//! The Media Session plays MP4 movies (mf/session.rs, mf/topology.rs, mf/movie.rs). The Source
//! Reader reads sound files and MP4s (mf/reader.rs).
//!
//! The engine's state follows the HTML media element it is modelled on (`State`, kept free of
//! the guest and tested here); winref's mfengine probe has Windows' answers.

#![allow(non_snake_case)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use crate::api::{implement, vtable, Ctx, Handler};
use crate::dshow_play::{self as play, frame_at, letterbox, now, Play, SEC};
use crate::mem::{self, ps, put32, putp, r16, r32, rp, w16, w32, w64, wp};

const S_FALSE: u32 = 1;
const E_FAIL: u32 = 0x8000_4005;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_POINTER: u32 = 0x8000_4003;
const E_INVALIDARG: u32 = 0x8007_0057;
const E_OUTOFMEMORY: u32 = 0x8007_000e;
/// The largest blob an attribute store takes (MF itself has no limit but memory: a blob past
/// this is the guest's bug, and copying gigabytes would end sake).
const MAX_BLOB: u32 = 64 << 20;

/// A copy of the guest's blob of `n` bytes at `p`: E_OUTOFMEMORY past MAX_BLOB.
fn blob(p: u32, n: u32) -> Result<Vec<u8>, u32> {
    if n == 0 {
        return Ok(Vec::new());
    }
    if n > MAX_BLOB {
        return Err(E_OUTOFMEMORY);
    }
    if p == 0 {
        return Err(E_POINTER);
    }
    Ok(mem::slice(p, n).to_vec())
}
const INSUFFICIENT_BUFFER: u32 = 0x8007_007a;
const MF_E_SHUTDOWN: u32 = 0xc00d_3e85;
const MF_E_ATTRIBUTENOTFOUND: u32 = 0xc00d_36e6;
const MF_E_INVALIDTYPE: u32 = 0xc00d_36bd;
const MF_E_INVALIDMEDIATYPE: u32 = 0xc00d_36b4;
/// TransferVideoFrame with nothing loaded (Windows' answer; the SDK's headers do not name it)
const NO_FRAME_TO_TRANSFER: u32 = 0xc00d_4e26;
const MF_E_INVALIDREQUEST: u32 = 0xc00d_36b2;
const MF_E_UNSUPPORTED_BYTESTREAM_TYPE: u32 = 0xc00d_36c4;

type Guid = [u8; 16];

fn guid_at(p: u32) -> Guid {
    mem::slice(p, 16).try_into().unwrap()
}
/// The first fields of a GUID: (Data1, Data2, Data3)
fn is(g: &Guid, d1: u32, d2: u16, d3: u16) -> bool {
    u32::from_le_bytes(g[0..4].try_into().unwrap()) == d1 && u16::from_le_bytes([g[4], g[5]]) == d2 && u16::from_le_bytes([g[6], g[7]]) == d3
}
fn is_unknown(g: &Guid) -> bool {
    g == &[0, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46]
}

/// A guest COM object's method `i`, called.
fn com(c: &mut Ctx, o: u32, i: u32, args: &[u32]) -> u32 {
    let f = rp(rp(o) + ps() * i);
    let mut a = vec![o];
    a.extend_from_slice(args);
    c.call(f, &a)
}
fn add_ref(c: &mut Ctx, o: u32) {
    if o != 0 {
        com(c, o, 1, &[]);
    }
}
fn release(c: &mut Ctx, o: u32) {
    if o != 0 {
        com(c, o, 2, &[]);
    }
}

/// A double argument: x86 stack slots `slot`, `slot + 1`; x64 argument `arg`.
fn dbl(c: &Ctx, slot: u32, arg: u32) -> f64 {
    if mem::x64() { f64::from_bits(c.xmm(arg)) } else { c.f64(slot) }
}
/// A 64-bit integer argument: x86 slots `slot`, `slot + 1`; x64 argument `arg`.
fn u64_arg(c: &Ctx, slot: u32, arg: u32) -> u64 {
    if mem::x64() { c.a64(arg) } else { c.a(slot) as u64 | (c.a(slot + 1) as u64) << 32 }
}

fn ok(_: &mut Ctx) -> u32 {
    0
}
fn notimpl(_: &mut Ctx) -> u32 {
    E_NOTIMPL
}
fn zero(_: &mut Ctx) -> u32 {
    0
}

/// A block of `words` pointer-sized words, word 0 the vtable.
fn object(vt: u32, words: u32) -> u32 {
    let b = mem::heap_alloc(ps() * words, true);
    wp(b, vt);
    b
}

// ---------------------------------------------------------------------------------------------
// IMFAttributes: a store of values by GUID, in the order they were first set

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    U32(u32),
    U64(u64),
    F64(f64),
    Guid(Guid),
    /// UTF-16, no NUL
    Str(Vec<u16>),
    Blob(Vec<u8>),
    /// a guest interface pointer (the store holds a reference)
    Unk(u32),
}

impl Val {
    /// MF_ATTRIBUTE_TYPE (the PROPVARIANT's VARTYPE)
    pub fn vt(&self) -> u16 {
        match self {
            Val::U32(_) => 19,
            Val::U64(_) => 21,
            Val::F64(_) => 5,
            Val::Guid(_) => 72,
            Val::Str(_) => 31,
            Val::Blob(_) => 0x1011,
            Val::Unk(_) => 13,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Store {
    pub items: Vec<(Guid, Val)>,
}

impl Store {
    pub fn get(&self, k: &Guid) -> Option<&Val> {
        self.items.iter().find(|i| &i.0 == k).map(|i| &i.1)
    }
    /// Sets `k` (in its place if it was there); the value it had.
    pub fn set(&mut self, k: Guid, v: Val) -> Option<Val> {
        match self.items.iter_mut().find(|i| i.0 == k) {
            Some(i) => Some(std::mem::replace(&mut i.1, v)),
            None => {
                self.items.push((k, v));
                None
            }
        }
    }
    pub fn delete(&mut self, k: &Guid) -> Option<Val> {
        let i = self.items.iter().position(|i| &i.0 == k)?;
        Some(self.items.remove(i).1)
    }
    /// Whether every item of `self` is in `other` with the same value.
    fn within(&self, other: &Store) -> bool {
        self.items.iter().all(|(k, v)| other.get(k) == Some(v))
    }
    /// IMFAttributes::Compare's MF_ATTRIBUTES_MATCH_TYPE: 0 our items, 1 their items, 2 all
    /// items, 3 the intersection, 4 the smaller set's items. None: no such type.
    pub fn matches(&self, other: &Store, how: u32) -> Option<bool> {
        Some(match how {
            0 => self.within(other),
            1 => other.within(self),
            2 => self.items.len() == other.items.len() && self.within(other),
            3 => self.items.iter().all(|(k, v)| other.get(k).is_none_or(|w| w == v)),
            4 => {
                if self.items.len() <= other.items.len() {
                    self.within(other)
                } else {
                    other.within(self)
                }
            }
            _ => return None,
        })
    }
}

/// The attribute stores, by block: (references, the store).
static ATTRS: Mutex<Option<HashMap<u32, (u32, Store)>>> = Mutex::new(None);

fn attrs<R>(b: u32, f: impl FnOnce(&mut (u32, Store)) -> R) -> Option<R> {
    ATTRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}
/// A copy of store `b` (None: not one of ours).
pub fn store_of(b: u32) -> Option<Store> {
    attrs(b, |a| a.1.clone())
}

fn new_attrs() -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    let b = object(*VT.get_or_init(attrs_vtable), 1);
    ATTRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, (1, Store::default()));
    b
}

/// A value (its reference taken) into PROPVARIANT `p`.
fn put_prop(c: &mut Ctx, p: u32, v: &Val) {
    mem::fill(p, 8 + 2 * ps(), 0);
    w16(p, v.vt());
    match v {
        Val::U32(x) => w32(p + 8, *x),
        Val::U64(x) => w64(p + 8, *x),
        Val::F64(x) => w64(p + 8, x.to_bits()),
        Val::Guid(g) => {
            let q = mem::heap_alloc(16, false);
            mem::write(q, g);
            wp(p + 8, q);
        }
        Val::Str(s) => wp(p + 8, task_wstr(s)),
        Val::Blob(b) => {
            let q = mem::heap_alloc(b.len().max(1) as u32, false);
            mem::write(q, b);
            w32(p + 8, b.len() as u32);
            wp(p + 8 + ps(), q);
        }
        Val::Unk(u) => {
            add_ref(c, *u);
            wp(p + 8, *u);
        }
    }
}
/// The value of PROPVARIANT `p` (MF_E_INVALIDTYPE: not a type attributes hold).
fn get_prop(p: u32) -> Result<Val, u32> {
    Ok(match r16(p) {
        19 => Val::U32(r32(p + 8)),
        21 => Val::U64(mem::r64(p + 8)),
        5 => Val::F64(f64::from_bits(mem::r64(p + 8))),
        72 => Val::Guid(guid_at(rp(p + 8))),
        31 => Val::Str(mem::wstr(rp(p + 8))),
        0x1011 => Val::Blob(blob(rp(p + 8 + ps()), r32(p + 8))?),
        13 => Val::Unk(rp(p + 8)),
        _ => return Err(MF_E_INVALIDTYPE),
    })
}
fn task_wstr(s: &[u16]) -> u32 {
    let q = mem::heap_alloc(2 * s.len() as u32 + 2, false);
    for (i, &ch) in s.iter().chain([&0]).enumerate() {
        w16(q + 2 * i as u32, ch);
    }
    q
}

/// The value of key (argument 1) of the store this is, or the error.
fn value(c: &Ctx) -> Result<Val, u32> {
    if c.a(1) == 0 {
        return Err(E_POINTER);
    }
    let k = guid_at(c.a(1));
    attrs(c.a(0), |a| a.1.get(&k).cloned()).flatten().ok_or(MF_E_ATTRIBUTENOTFOUND)
}
/// Sets key (argument 1) of this store; the value it replaces is released.
fn set_value(c: &mut Ctx, v: Val) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let k = guid_at(c.a(1));
    if let Val::Unk(u) = v {
        add_ref(c, u);
    }
    let old = attrs(c.a(0), |a| a.1.set(k, v)).flatten();
    if let Some(Val::Unk(u)) = old {
        release(c, u);
    }
    0
}
macro_rules! getter {
    ($name:ident, $pat:pat => $put:expr) => {
        fn $name(c: &mut Ctx) -> u32 {
            let out = c.a(2);
            match value(c) {
                Ok($pat) => {
                    if out == 0 {
                        return E_POINTER;
                    }
                    #[allow(clippy::redundant_closure_call)]
                    ($put)(out);
                    0
                }
                Ok(_) => MF_E_INVALIDTYPE,
                Err(e) => e,
            }
        }
    };
}
getter!(at_get_u32, Val::U32(v) => |o| w32(o, v));
getter!(at_get_u64, Val::U64(v) => |o| w64(o, v));
getter!(at_get_f64, Val::F64(v) => |o| w64(o, v.to_bits()));
getter!(at_get_guid, Val::Guid(g) => |o| mem::write(o, &g));
getter!(at_get_strlen, Val::Str(s) => |o| w32(o, s.len() as u32));
getter!(at_get_blobsize, Val::Blob(b) => |o: u32| w32(o, b.len() as u32));

fn at_qi(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let g = guid_at(iid);
    // IMFAttributes {2cd2d921-c447-44a7-a13c-4adabfc247e3}
    if is_unknown(&g) || is(&g, 0x2cd2_d921, 0xc447, 0x44a7) {
        attrs(this, |a| a.0 += 1);
        putp(out, this);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn at_add_ref(c: &mut Ctx) -> u32 {
    attrs(c.a(0), |a| {
        a.0 += 1;
        a.0
    })
    .unwrap_or(1)
}
fn at_release(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let n = attrs(b, |a| {
        a.0 = a.0.saturating_sub(1);
        a.0
    })
    .unwrap_or(0);
    if n == 0 {
        let gone = ATTRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&b);
        if let Some((_, s)) = gone {
            for (_, v) in s.items {
                if let Val::Unk(u) = v {
                    release(c, u);
                }
            }
            mem::heap_free(b);
        }
    }
    n
}
/// GetItem(key, PROPVARIANT *v): v may be NULL (whether it is there)
fn at_get_item(c: &mut Ctx) -> u32 {
    let out = c.a(2);
    match value(c) {
        Ok(v) => {
            if out != 0 {
                put_prop(c, out, &v);
            }
            0
        }
        Err(e) => e,
    }
}
fn at_get_type(c: &mut Ctx) -> u32 {
    let out = c.a(2);
    match value(c) {
        Ok(v) => {
            if out == 0 {
                return E_POINTER;
            }
            w32(out, v.vt() as u32);
            0
        }
        Err(e) => e,
    }
}
/// CompareItem(key, PROPVARIANT, BOOL *same): FALSE for a key not there
fn at_compare_item(c: &mut Ctx) -> u32 {
    let (pv, out) = (c.a(2), c.a(3));
    if pv == 0 || out == 0 {
        return E_POINTER;
    }
    let same = match (value(c), get_prop(pv)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    w32(out, same as u32);
    0
}
/// Compare(IMFAttributes *theirs, MF_ATTRIBUTES_MATCH_TYPE, BOOL *same): ours only
fn at_compare(c: &mut Ctx) -> u32 {
    let (theirs, how, out) = (c.a(1), c.a(2), c.a(3));
    if theirs == 0 || out == 0 {
        return E_POINTER;
    }
    let (Some(a), Some(b)) = (store_of(c.a(0)), store_of(theirs)) else { return E_NOTIMPL };
    match a.matches(&b, how) {
        Some(m) => {
            w32(out, m as u32);
            0
        }
        None => E_INVALIDARG,
    }
}
/// GetString(key, WCHAR *buf, UINT32 chars, UINT32 *length)
fn at_get_string(c: &mut Ctx) -> u32 {
    let (buf, cap, len) = (c.a(2), c.a(3), c.a(4));
    match value(c) {
        Ok(Val::Str(s)) => {
            if len != 0 {
                w32(len, s.len() as u32);
            }
            if buf == 0 {
                return E_POINTER;
            }
            if (cap as usize) < s.len() + 1 {
                return INSUFFICIENT_BUFFER;
            }
            for (i, &ch) in s.iter().chain([&0]).enumerate() {
                w16(buf + 2 * i as u32, ch);
            }
            0
        }
        Ok(_) => MF_E_INVALIDTYPE,
        Err(e) => e,
    }
}
/// GetAllocatedString(key, WCHAR **s, UINT32 *length)
fn at_get_alloc_string(c: &mut Ctx) -> u32 {
    let (out, len) = (c.a(2), c.a(3));
    match value(c) {
        Ok(Val::Str(s)) => {
            if out == 0 || len == 0 {
                return E_POINTER;
            }
            putp(out, task_wstr(&s));
            w32(len, s.len() as u32);
            0
        }
        Ok(_) => MF_E_INVALIDTYPE,
        Err(e) => e,
    }
}
/// GetBlob(key, BYTE *buf, UINT32 size, UINT32 *got)
fn at_get_blob(c: &mut Ctx) -> u32 {
    let (buf, cap, got) = (c.a(2), c.a(3), c.a(4));
    match value(c) {
        Ok(Val::Blob(b)) => {
            if buf == 0 && !b.is_empty() {
                return E_POINTER;
            }
            if (cap as usize) < b.len() {
                return INSUFFICIENT_BUFFER;
            }
            mem::write(buf, &b);
            if got != 0 {
                w32(got, b.len() as u32);
            }
            0
        }
        Ok(_) => MF_E_INVALIDTYPE,
        Err(e) => e,
    }
}
/// GetAllocatedBlob(key, BYTE **buf, UINT32 *size)
fn at_get_alloc_blob(c: &mut Ctx) -> u32 {
    let (out, size) = (c.a(2), c.a(3));
    match value(c) {
        Ok(Val::Blob(b)) => {
            if out == 0 || size == 0 {
                return E_POINTER;
            }
            let q = mem::heap_alloc(b.len().max(1) as u32, false);
            mem::write(q, &b);
            putp(out, q);
            w32(size, b.len() as u32);
            0
        }
        Ok(_) => MF_E_INVALIDTYPE,
        Err(e) => e,
    }
}
/// GetUnknown(key, REFIID, void **out): the object's QueryInterface
fn at_get_unknown(c: &mut Ctx) -> u32 {
    let (iid, out) = (c.a(2), c.a(3));
    match value(c) {
        Ok(Val::Unk(u)) => {
            if out == 0 {
                return E_POINTER;
            }
            if u == 0 {
                putp(out, 0);
                return E_NOINTERFACE;
            }
            com(c, u, 0, &[iid, out])
        }
        Ok(_) => MF_E_INVALIDTYPE,
        Err(e) => e,
    }
}
fn at_set_item(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 {
        return E_POINTER;
    }
    match get_prop(c.a(2)) {
        Ok(v) => set_value(c, v),
        Err(e) => e,
    }
}
fn at_delete(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let k = guid_at(c.a(1));
    if let Some(Val::Unk(u)) = attrs(c.a(0), |a| a.1.delete(&k)).flatten() {
        release(c, u);
    }
    0
}
fn at_delete_all(c: &mut Ctx) -> u32 {
    let items = attrs(c.a(0), |a| std::mem::take(&mut a.1.items)).unwrap_or_default();
    for (_, v) in items {
        if let Val::Unk(u) = v {
            release(c, u);
        }
    }
    0
}
fn at_set_u32(c: &mut Ctx) -> u32 {
    let v = c.a(2);
    set_value(c, Val::U32(v))
}
fn at_set_u64(c: &mut Ctx) -> u32 {
    let v = u64_arg(c, 2, 2);
    set_value(c, Val::U64(v))
}
fn at_set_f64(c: &mut Ctx) -> u32 {
    let v = dbl(c, 2, 2);
    set_value(c, Val::F64(v))
}
fn at_set_guid(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 {
        return E_POINTER;
    }
    let g = guid_at(c.a(2));
    set_value(c, Val::Guid(g))
}
fn at_set_string(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 {
        return E_POINTER;
    }
    let s = mem::wstr(c.a(2));
    set_value(c, Val::Str(s))
}
fn at_set_blob(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(2), c.a(3));
    if p == 0 && n != 0 {
        return E_POINTER;
    }
    match blob(p, n) {
        Ok(b) => set_value(c, Val::Blob(b)),
        Err(e) => e,
    }
}
fn at_set_unknown(c: &mut Ctx) -> u32 {
    let u = c.a(2);
    set_value(c, Val::Unk(u))
}
fn at_count(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put32(c.a(1), attrs(c.a(0), |a| a.1.items.len() as u32).unwrap_or(0));
    0
}
/// GetItemByIndex(index, GUID *key, PROPVARIANT *v)
fn at_by_index(c: &mut Ctx) -> u32 {
    let (i, key, out) = (c.a(1), c.a(2), c.a(3));
    let Some((k, v)) = attrs(c.a(0), |a| a.1.items.get(i as usize).cloned()).flatten() else { return E_INVALIDARG };
    if key != 0 {
        mem::write(key, &k);
    }
    if out != 0 {
        put_prop(c, out, &v);
    }
    0
}
/// CopyAllItems(IMFAttributes *to): into one of ours (what it had goes)
fn at_copy_all(c: &mut Ctx) -> u32 {
    let to = c.a(1);
    if to == 0 {
        return E_POINTER;
    }
    let Some(s) = store_of(c.a(0)) else { return E_FAIL };
    if store_of(to).is_none() {
        return E_NOTIMPL;
    }
    for (_, v) in &s.items {
        if let Val::Unk(u) = v {
            add_ref(c, *u);
        }
    }
    let old = attrs(to, |a| std::mem::replace(&mut a.1, s)).unwrap_or_default();
    for (_, v) in old.items {
        if let Val::Unk(u) = v {
            release(c, u);
        }
    }
    0
}

/// IMFAttributes' 33 methods (x86 argument bytes)
fn attrs_methods() -> Vec<(Handler, u32)> {
    vec![
        (at_qi as Handler, 12),
        (at_add_ref, 4),
        (at_release, 4),
        (at_get_item, 12),
        (at_get_type, 12),
        (at_compare_item, 16),
        (at_compare, 16),
        (at_get_u32, 12),
        (at_get_u64, 12),
        (at_get_f64, 12),
        (at_get_guid, 12),
        (at_get_strlen, 12),
        (at_get_string, 20),
        (at_get_alloc_string, 16),
        (at_get_blobsize, 12),
        (at_get_blob, 20),
        (at_get_alloc_blob, 16),
        (at_get_unknown, 16),
        (at_set_item, 12),
        (at_delete, 8),
        (at_delete_all, 4),
        (at_set_u32, 12),
        (at_set_u64, 16),
        (at_set_f64, 16),
        (at_set_guid, 12),
        (at_set_string, 12),
        (at_set_blob, 16),
        (at_set_unknown, 12),
        (ok, 4), // LockStore
        (ok, 4), // UnlockStore
        (at_count, 8),
        (at_by_index, 16),
        (at_copy_all, 8),
    ]
}
fn attrs_vtable() -> u32 {
    vtable("IMFAttributes", &attrs_methods())
}

/// MFCreateAttributes(IMFAttributes **out, UINT32 initial size)
fn MFCreateAttributes(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        return E_POINTER;
    }
    putp(c.a(0), new_attrs());
    0
}

// ---------------------------------------------------------------------------------------------
// IMFDXGIDeviceManager: the device the engine's frames go to (theirs: the textures name it)

struct Manager {
    refs: u32,
    token: u32,
    device: u32,
}
static MANAGERS: Mutex<Option<HashMap<u32, Manager>>> = Mutex::new(None);

fn manager<R>(b: u32, f: impl FnOnce(&mut Manager) -> R) -> Option<R> {
    MANAGERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}

fn mg_qi(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let g = guid_at(c.a(1));
    // IMFDXGIDeviceManager {eb533d5d-2db6-40f8-97a9-494692014f07}
    if is_unknown(&g) || is(&g, 0xeb53_3d5d, 0x2db6, 0x40f8) {
        manager(this, |m| m.refs += 1);
        putp(out, this);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn mg_add_ref(c: &mut Ctx) -> u32 {
    manager(c.a(0), |m| {
        m.refs += 1;
        m.refs
    })
    .unwrap_or(1)
}
fn mg_release(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let (n, dev) = manager(b, |m| {
        m.refs = m.refs.saturating_sub(1);
        (m.refs, m.device)
    })
    .unwrap_or((0, 0));
    if n == 0 && MANAGERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&b).is_some() {
        release(c, dev);
        mem::heap_free(b);
    }
    n
}
/// ResetDevice(IUnknown *device, UINT token)
fn mg_reset(c: &mut Ctx) -> u32 {
    let (b, dev, token) = (c.a(0), c.a(1), c.a(2));
    if dev == 0 {
        return E_INVALIDARG;
    }
    let Some(old) = manager(b, |m| if m.token == token { Some(m.device) } else { None }).flatten() else { return E_INVALIDARG };
    add_ref(c, dev);
    manager(b, |m| m.device = dev);
    release(c, old);
    0
}
/// OpenDeviceHandle(HANDLE *h): one handle, the device's
fn mg_open(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    if manager(c.a(0), |m| m.device).unwrap_or(0) == 0 {
        return 0xc00d_36b6; // MF_E_DXGI_DEVICE_NOT_INITIALIZED
    }
    putp(c.a(1), 1);
    0
}
/// LockDevice(HANDLE, REFIID, void **out, BOOL block) / GetVideoService(HANDLE, REFIID, void **):
/// the device's QueryInterface
fn mg_device_qi(c: &mut Ctx) -> u32 {
    let (iid, out) = (c.a(2), c.a(3));
    if out == 0 {
        return E_POINTER;
    }
    let dev = manager(c.a(0), |m| m.device).unwrap_or(0);
    if dev == 0 {
        putp(out, 0);
        return 0xc00d_36b6;
    }
    com(c, dev, 0, &[iid, out])
}

fn MFCreateDXGIDeviceManager(c: &mut Ctx) -> u32 {
    let (token, out) = (c.a(0), c.a(1));
    if token == 0 || out == 0 {
        return E_POINTER;
    }
    static VT: OnceLock<u32> = OnceLock::new();
    let vt = *VT.get_or_init(|| {
        vtable(
            "IMFDXGIDeviceManager",
            &[
                (mg_qi as Handler, 12),
                (mg_add_ref, 4),
                (mg_release, 4),
                (ok, 8),            // CloseDeviceHandle
                (mg_device_qi, 16), // GetVideoService
                (mg_device_qi, 20), // LockDevice
                (mg_open, 8),       // OpenDeviceHandle
                (mg_reset, 12),     // ResetDevice
                (ok, 8),            // TestDevice
                (ok, 12),           // UnlockDevice
            ],
        )
    });
    let b = object(vt, 1);
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x2b6d_7a01);
    let t = NEXT.fetch_add(0x101, std::sync::atomic::Ordering::Relaxed);
    MANAGERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, Manager { refs: 1, token: t, device: 0 });
    w32(token, t);
    putp(out, b);
    0
}

// ---------------------------------------------------------------------------------------------
// IMFMediaError: a block of the vtable, then the code (u16), the HRESULT and references

fn new_error(code: u16, hr: u32) -> u32 {
    fn er_qi(c: &mut Ctx) -> u32 {
        let (this, out) = (c.a(0), c.a(2));
        if out == 0 {
            return E_POINTER;
        }
        let g = guid_at(c.a(1));
        // IMFMediaError {fc0e10d2-ab2a-4501-a951-06bb1075184c}
        if is_unknown(&g) || is(&g, 0xfc0e_10d2, 0xab2a, 0x4501) {
            er_add_ref(c);
            putp(out, this);
            return 0;
        }
        putp(out, 0);
        E_NOINTERFACE
    }
    fn er_add_ref(c: &mut Ctx) -> u32 {
        let p = c.a(0) + ps() + 8;
        w32(p, r32(p) + 1);
        r32(p)
    }
    fn er_release(c: &mut Ctx) -> u32 {
        let p = c.a(0) + ps() + 8;
        let n = r32(p).saturating_sub(1);
        w32(p, n);
        if n == 0 {
            mem::heap_free(c.a(0));
        }
        n
    }
    fn er_code(c: &mut Ctx) -> u32 {
        r16(c.a(0) + ps()) as u32
    }
    fn er_hr(c: &mut Ctx) -> u32 {
        r32(c.a(0) + ps() + 4)
    }
    fn er_set_code(c: &mut Ctx) -> u32 {
        if c.a(1) > 4 {
            return E_INVALIDARG;
        }
        w16(c.a(0) + ps(), c.a(1) as u16);
        0
    }
    fn er_set_hr(c: &mut Ctx) -> u32 {
        w32(c.a(0) + ps() + 4, c.a(1));
        0
    }
    static VT: OnceLock<u32> = OnceLock::new();
    let vt = *VT.get_or_init(|| {
        vtable("IMFMediaError", &[(er_qi as Handler, 12), (er_add_ref, 4), (er_release, 4), (er_code, 4), (er_hr, 4), (er_set_code, 8), (er_set_hr, 8)])
    });
    let b = object(vt, 3);
    w16(b + ps(), code);
    w32(b + ps() + 4, hr);
    w32(b + ps() + 8, 1);
    b
}

// ---------------------------------------------------------------------------------------------
// the engine's state, as the HTML media element's

/// MF_MEDIA_ENGINE_EVENT
pub mod ev {
    pub const LOADSTART: u32 = 1;
    pub const ERROR: u32 = 5;
    pub const EMPTIED: u32 = 6;
    pub const PLAY: u32 = 8;
    pub const PAUSE: u32 = 9;
    pub const LOADEDMETADATA: u32 = 10;
    pub const LOADEDDATA: u32 = 11;
    pub const PLAYING: u32 = 13;
    pub const CANPLAY: u32 = 14;
    pub const CANPLAYTHROUGH: u32 = 15;
    pub const SEEKING: u32 = 16;
    pub const SEEKED: u32 = 17;
    pub const TIMEUPDATE: u32 = 18;
    pub const ENDED: u32 = 19;
    pub const RATECHANGE: u32 = 20;
    pub const DURATIONCHANGE: u32 = 21;
    pub const VOLUMECHANGE: u32 = 22;
    pub const FIRSTFRAMEREADY: u32 = 1009;
}

/// What the engine says about itself (what its getters answer), and the events to send.
#[derive(Clone, Debug, PartialEq)]
pub struct State {
    /// MF_MEDIA_ENGINE_READY: 0 nothing, 4 enough data
    pub ready: u16,
    /// MF_MEDIA_ENGINE_NETWORK: 0 empty, 1 idle, 2 loading, 3 no source
    pub network: u16,
    pub paused: bool,
    pub ended: bool,
    pub seeking: bool,
    pub autoplay: bool,
    pub looping: bool,
    pub muted: bool,
    pub volume: f64,
    pub rate: f64,
    pub default_rate: f64,
    /// MF_MEDIA_ENGINE_ERR (0: none) and its HRESULT
    pub error: (u16, u32),
    /// in seconds (NaN: no movie)
    pub duration: f64,
    pub video: Option<(u32, u32)>,
    pub audio: bool,
    /// (event, param 1, param 2)
    pub events: VecDeque<(u32, u32, u32)>,
}

impl Default for State {
    fn default() -> State {
        State {
            ready: 0,
            network: 0,
            paused: true,
            ended: false,
            seeking: false,
            autoplay: false,
            looping: false,
            muted: false,
            volume: 1.0,
            rate: 1.0,
            default_rate: 1.0,
            error: (0, 0),
            duration: f64::NAN,
            video: None,
            audio: false,
            events: VecDeque::new(),
        }
    }
}

impl State {
    fn event(&mut self, e: u32) {
        self.events.push_back((e, 0, 0));
    }
    /// A new source: what there was goes (the load algorithm).
    pub fn reset(&mut self) {
        if self.network != 0 {
            self.event(ev::EMPTIED);
        }
        self.paused = true;
        self.ended = false;
        self.seeking = false;
        self.ready = 0;
        self.error = (0, 0);
        self.duration = f64::NAN;
        self.video = None;
        self.audio = false;
        self.network = 2;
        self.event(ev::LOADSTART);
    }
    /// The source could not be played.
    pub fn failed(&mut self, hr: u32) {
        self.error = (4, hr);
        self.network = 3;
        self.events.push_back((ev::ERROR, 4, hr));
    }
    /// The source loaded (all of it is there at once). Whether it runs now (autoplay).
    pub fn loaded(&mut self, duration: f64, video: Option<(u32, u32)>, audio: bool) -> bool {
        self.duration = duration;
        self.video = video;
        self.audio = audio;
        self.network = 1;
        self.event(ev::DURATIONCHANGE);
        self.event(ev::LOADEDMETADATA);
        self.ready = 2;
        self.event(ev::LOADEDDATA);
        if video.is_some() {
            self.event(ev::FIRSTFRAMEREADY);
        }
        self.ready = 4;
        self.event(ev::CANPLAY);
        if self.autoplay && self.paused {
            self.paused = false;
            self.event(ev::PLAY);
        }
        if !self.paused {
            self.event(ev::PLAYING);
        }
        self.event(ev::CANPLAYTHROUGH);
        !self.paused
    }
    /// Play: (whether it goes back to the start first, whether it runs now)
    pub fn play(&mut self) -> (bool, bool) {
        let rewind = self.ended && self.ready > 0;
        if rewind {
            self.seek();
        }
        if self.paused {
            self.paused = false;
            self.event(ev::PLAY);
            if self.ready >= 3 {
                self.event(ev::PLAYING);
            }
        }
        (rewind, self.ready >= 3)
    }
    pub fn pause(&mut self) {
        if !self.paused {
            self.paused = true;
            self.event(ev::TIMEUPDATE);
            self.event(ev::PAUSE);
        }
    }
    /// A seek (done at once). False: nothing to seek in.
    pub fn seek(&mut self) -> bool {
        if self.ready == 0 {
            return false;
        }
        self.ended = false;
        self.seeking = true;
        self.event(ev::SEEKING);
        self.seeking = false;
        self.event(ev::TIMEUPDATE);
        self.event(ev::SEEKED);
        true
    }
    /// The end was reached. Whether it goes on from the start (looping).
    pub fn end(&mut self) -> bool {
        if self.looping {
            self.seek();
            return true;
        }
        self.event(ev::TIMEUPDATE);
        if !self.paused {
            self.paused = true;
            self.event(ev::PAUSE);
        }
        self.ended = true;
        self.event(ev::ENDED);
        false
    }
}

// ---------------------------------------------------------------------------------------------
// IMFMediaEngine(Ex): one block (the vtable word), the state by block in ENGINES

struct Engine {
    refs: u32,
    st: State,
    notify: u32,
    shut: bool,
    /// the source set (UTF-16)
    src: Option<Vec<u16>>,
    /// a movie plays (dshow_play's PLAYS holds it, by the block)
    movie: bool,
    /// the frame OnVideoStreamTick last told of
    ticked: Option<u32>,
    category: u32,
    role: u32,
    /// the events thread runs
    thread: bool,
    /// TransferVideoFrame's pictures
    frames: Frames,
}

static ENGINES: Mutex<Option<HashMap<u32, Engine>>> = Mutex::new(None);

fn engine<R>(b: u32, f: impl FnOnce(&mut Engine) -> R) -> Option<R> {
    let mut g = ENGINES.lock().unwrap_or_else(|e| e.into_inner());
    let e = g.get_or_insert_with(HashMap::new).get_mut(&b)?;
    let r = f(e);
    // (its events thread sleeps until there is something for it)
    if e.thread && (e.shut || !e.st.events.is_empty()) {
        EVENTS_CV.notify_all();
    }
    Some(r)
}
fn shut(b: u32) -> bool {
    engine(b, |e| e.shut).unwrap_or(true)
}

/// Where the movie of engine `b` is: (position, frames per second, frames, stream, w, h).
fn position(b: u32) -> Option<(u64, (u32, u32), u32, u32, u32, u32)> {
    let t = now();
    play::read(|m| m.get(&b).map(|p| (p.tl.position(t), p.movie.rate, p.movie.frames, p.movie.stream, p.movie.w, p.movie.h)))
}
/// The movie's volume as IBasicAudio's (1/100 dB).
fn centibels(v: f64, muted: bool) -> i32 {
    if muted || v <= 0.0 || v.is_nan() { -10000 } else { (2000.0 * v.min(1.0).log10()).round().max(-10000.0) as i32 }
}
fn apply_volume(b: u32) {
    let Some(v) = engine(b, |e| centibels(e.st.volume, e.st.muted)) else { return };
    play::with(|m| {
        if let Some(p) = m.get_mut(&b) {
            p.volume = v;
        }
    });
}
fn timeline(b: u32, f: impl FnOnce(&mut play::Timeline, u64)) {
    let t = now();
    play::with(|m| {
        if let Some(p) = m.get_mut(&b) {
            f(&mut p.tl, t)
        }
    });
}

/// The movie of engine `k` reached its end (dshow_play's movie thread): whether `k` is one.
pub fn ended(k: u32) -> bool {
    let Some(again) = engine(k, |e| {
        e.ticked = None;
        e.st.end()
    }) else {
        return false;
    };
    timeline(k, |tl, t| {
        if again {
            tl.seek(t, 0);
            tl.run(t);
        } else {
            tl.pause(t);
        }
    });
    true
}

/// The thread that tells the game's IMFMediaEngineNotify (EventNotify) the events, in order,
/// until the engine shuts down. Its start "function" is this trap; the engine block its parameter.
/// It sleeps until an event comes (EVENTS_CV), not in a poll.
fn events_thread(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    loop {
        match next_event(b, std::time::Duration::from_millis(250)) {
            Some(Ok((n, Some((code, p1, p2))))) => {
                com(c, n, 3, &[code, p1, p2]);
            }
            Some(Ok((_, None))) => {
                crate::snap::point();
            }
            _ => break,
        }
    }
    let n = engine(b, |e| {
        e.thread = false;
        std::mem::take(&mut e.notify)
    })
    .unwrap_or(0);
    release(c, n);
    retire(b);
    crate::k32::exit_thread(0)
}

/// Wakes the events threads: an engine's events or its shutdown changed (and a save state
/// wants them at their safe point).
static EVENTS_CV: std::sync::Condvar = std::sync::Condvar::new();
pub(crate) fn snap_wake() {
    drop(ENGINES.lock());
    EVENTS_CV.notify_all();
    session::snap_wake();
}

/// The next event of engine `b` for its events thread: Err when it shut down (None: no such
/// engine), Ok((notify, None)) when none came in `wait` or a save state is being taken.
fn next_event(b: u32, wait: std::time::Duration) -> Option<Result<(u32, Option<(u32, u32, u32)>), ()>> {
    let mut g = ENGINES.lock().unwrap_or_else(|e| e.into_inner());
    let mut waited = false;
    loop {
        let e = g.get_or_insert_with(HashMap::new).get_mut(&b)?;
        if e.shut {
            return Some(Err(()));
        }
        if let Some(ev) = e.st.events.pop_front() {
            return Some(Ok((e.notify, Some(ev))));
        }
        if waited || kura::snap::pausing() {
            return Some(Ok((e.notify, None)));
        }
        g = EVENTS_CV.wait_timeout(g, wait).unwrap_or_else(|e| e.into_inner()).0;
        waited = true;
    }
}

/// Engine `b` goes for good once nothing holds it (released by the game, its events thread
/// gone): its state and its block. Whether it went.
fn retire(b: u32) -> bool {
    let gone = {
        let mut g = ENGINES.lock().unwrap_or_else(|e| e.into_inner());
        let m = g.get_or_insert_with(HashMap::new);
        match m.get(&b) {
            Some(e) if e.refs == 0 && !e.thread && e.shut => m.remove(&b).is_some(),
            _ => false,
        }
    };
    if gone {
        mem::heap_free(b);
        #[cfg(test)]
        RETIRED.lock().unwrap_or_else(|e| e.into_inner()).push(b);
    }
    gone
}
/// (tests: the blocks `retire` let go, in order)
#[cfg(test)]
static RETIRED: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn start_events(b: u32) {
    if engine(b, |e| !std::mem::replace(&mut e.thread, true)) != Some(true) {
        return;
    }
    static ENTRY: OnceLock<u32> = OnceLock::new();
    let entry = *ENTRY.get_or_init(|| crate::api::host_fn("mf_events_thread", events_thread, 4));
    crate::spawn_service(entry, b);
}

fn en_qi(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let g = guid_at(c.a(1));
    // IMFMediaEngine {98a1b0bb-03eb-4935-ae7c-93c1fa0e1c93}, IMFMediaEngineEx {83015ead-b1e6-40d0-...}
    if is_unknown(&g) || is(&g, 0x98a1_b0bb, 0x03eb, 0x4935) || is(&g, 0x8301_5ead, 0xb1e6, 0x40d0) {
        en_add_ref(c);
        putp(out, this);
        return 0;
    }
    if crate::env_on!("SAKE_TRACE_COM") {
        eprintln!("sake: IMFMediaEngine QueryInterface {:08x}: no", r32(c.a(1)));
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn en_add_ref(c: &mut Ctx) -> u32 {
    engine(c.a(0), |e| {
        e.refs += 1;
        e.refs
    })
    .unwrap_or(1)
}
fn en_release(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let n = engine(b, |e| {
        e.refs = e.refs.saturating_sub(1);
        e.refs
    })
    .unwrap_or(0);
    if n == 0 {
        shutdown(c, b);
        // (with its events thread still on its way out, that thread lets it go)
        retire(b);
    }
    n
}
fn shutdown(c: &mut Ctx, b: u32) {
    let had = engine(b, |e| {
        let had = !e.shut;
        e.shut = true;
        e.st.events.clear();
        // (no thread to let the callback go: here)
        let n = if e.thread { 0 } else { std::mem::take(&mut e.notify) };
        (had, n)
    });
    if let Some((true, n)) = had {
        play::end(b);
        release(c, n);
    }
}
fn en_shutdown(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    shutdown(c, b);
    0
}

/// A guest path of a source URL ("file:///C:/x.mp4" or a path).
fn source_path(u: &[u16]) -> String {
    let s = String::from_utf16_lossy(u);
    let s = match s.strip_prefix("file:///").or_else(|| s.strip_prefix("file://")) {
        Some(p) => p.replace('/', "\\"),
        None => s,
    };
    s.replace("%20", " ")
}

/// SetSource(BSTR url) / Load(): its `.vnv` plays, or it fails to load
fn load(b: u32) {
    play::end(b);
    let Some(src) = engine(b, |e| {
        e.movie = false;
        e.ticked = None;
        e.st.reset();
        e.src.clone()
    }) else {
        return;
    };
    let w = src.as_deref().map(source_path).unwrap_or_default();
    let movie = if w.is_empty() { None } else { play::Movie::find(&w) };
    let Some(movie) = movie else {
        let there = !w.is_empty() && crate::files::resolve(&w).is_some();
        if there {
            eprintln!("sake: movie {w}: not played (no .vnv to decode, see masu pack)");
        }
        engine(b, |e| e.st.failed(if there { MF_E_UNSUPPORTED_BYTESTREAM_TYPE } else { 0x8007_0002 }));
        return;
    };
    eprintln!("sake: movie {w}: {}x{} {} frames (Media Engine)", movie.w, movie.h, movie.frames);
    let (dur, video, audio) = (movie.duration as f64 / SEC as f64, Some((movie.w, movie.h)), movie.sound.is_some());
    let mut p = Play::new(movie);
    p.place.visible = false;
    play::start(b, p);
    apply_volume(b);
    let run = engine(b, |e| {
        e.movie = true;
        e.st.loaded(dur, video, audio)
    });
    if run == Some(true) {
        timeline(b, |tl, t| tl.run(t));
    }
}
fn en_set_source(c: &mut Ctx) -> u32 {
    let (b, url) = (c.a(0), c.a(1));
    let s = if url == 0 { None } else { Some(mem::wstr(url)) };
    engine(b, |e| e.src = s);
    load(b);
    0
}
fn en_load(c: &mut Ctx) -> u32 {
    load(c.a(0));
    0
}
fn en_get_source(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let s = engine(c.a(0), |e| e.src.clone()).flatten();
    putp(c.a(1), s.map_or(0, |s| crate::misc::bstr_alloc(&s)));
    0
}
fn en_get_error(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let (code, hr) = engine(c.a(0), |e| e.st.error).unwrap_or((0, 0));
    putp(c.a(1), if code == 0 { 0 } else { new_error(code, hr) });
    0
}
fn en_set_error(c: &mut Ctx) -> u32 {
    let code = c.a(1);
    if code > 4 {
        return E_INVALIDARG;
    }
    engine(c.a(0), |e| e.st.error = (code as u16, 0));
    0
}
fn en_network(c: &mut Ctx) -> u32 {
    engine(c.a(0), |e| e.st.network as u32).unwrap_or(0)
}
fn en_ready(c: &mut Ctx) -> u32 {
    engine(c.a(0), |e| e.st.ready as u32).unwrap_or(0)
}
fn en_seeking(c: &mut Ctx) -> u32 {
    engine(c.a(0), |e| e.st.seeking as u32).unwrap_or(0)
}
fn en_paused(c: &mut Ctx) -> u32 {
    engine(c.a(0), |e| e.st.paused as u32).unwrap_or(1)
}
fn en_ended(c: &mut Ctx) -> u32 {
    engine(c.a(0), |e| e.st.ended as u32).unwrap_or(0)
}
fn en_get_autoplay(c: &mut Ctx) -> u32 {
    engine(c.a(0), |e| e.st.autoplay as u32).unwrap_or(0)
}
fn en_set_autoplay(c: &mut Ctx) -> u32 {
    let v = c.a(1) != 0;
    engine(c.a(0), |e| e.st.autoplay = v);
    0
}
fn en_get_loop(c: &mut Ctx) -> u32 {
    engine(c.a(0), |e| e.st.looping as u32).unwrap_or(0)
}
fn en_set_loop(c: &mut Ctx) -> u32 {
    let v = c.a(1) != 0;
    engine(c.a(0), |e| e.st.looping = v);
    0
}
fn en_get_muted(c: &mut Ctx) -> u32 {
    engine(c.a(0), |e| e.st.muted as u32).unwrap_or(0)
}
fn en_set_muted(c: &mut Ctx) -> u32 {
    let (b, v) = (c.a(0), c.a(1) != 0);
    engine(b, |e| {
        if e.st.muted != v {
            e.st.muted = v;
            e.st.event(ev::VOLUMECHANGE);
        }
    });
    apply_volume(b);
    0
}
fn en_has_video(c: &mut Ctx) -> u32 {
    engine(c.a(0), |e| e.st.video.is_some() as u32).unwrap_or(0)
}
fn en_has_audio(c: &mut Ctx) -> u32 {
    engine(c.a(0), |e| e.st.audio as u32).unwrap_or(0)
}
fn ret_f64(c: &mut Ctx, v: f64) -> u32 {
    c.st0 = Some(v);
    0
}
fn en_time(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let t = position(b).map_or(0.0, |p| p.0 as f64 / SEC as f64);
    ret_f64(c, t)
}
fn en_set_time(c: &mut Ctx) -> u32 {
    let (b, to) = (c.a(0), dbl(c, 1, 1));
    seek_to(b, to)
}
/// SeekEx(double, MF_MEDIA_ENGINE_SEEK_MODE)
fn en_seek_ex(c: &mut Ctx) -> u32 {
    let (b, to) = (c.a(0), dbl(c, 1, 1));
    seek_to(b, to)
}
fn seek_to(b: u32, to: f64) -> u32 {
    let Some((true, dur)) = engine(b, |e| {
        let ok = e.st.seek();
        if ok {
            e.ticked = None;
        }
        (ok, e.st.duration)
    }) else {
        return MF_E_INVALIDREQUEST;
    };
    let to = if to.is_nan() { 0.0 } else { to.clamp(0.0, if dur.is_nan() { 0.0 } else { dur }) };
    timeline(b, |tl, t| tl.seek(t, (to * SEC as f64) as u64));
    0
}
fn en_start_time(c: &mut Ctx) -> u32 {
    ret_f64(c, 0.0)
}
fn en_duration(c: &mut Ctx) -> u32 {
    let d = engine(c.a(0), |e| e.st.duration).unwrap_or(f64::NAN);
    ret_f64(c, d)
}
fn en_default_rate(c: &mut Ctx) -> u32 {
    let r = engine(c.a(0), |e| e.st.default_rate).unwrap_or(1.0);
    ret_f64(c, r)
}
fn en_set_default_rate(c: &mut Ctx) -> u32 {
    let (b, r) = (c.a(0), dbl(c, 1, 1));
    engine(b, |e| {
        e.st.default_rate = r;
        e.st.event(ev::RATECHANGE);
    });
    0
}
fn en_rate(c: &mut Ctx) -> u32 {
    let r = engine(c.a(0), |e| e.st.rate).unwrap_or(1.0);
    ret_f64(c, r)
}
fn en_set_rate(c: &mut Ctx) -> u32 {
    let (b, r) = (c.a(0), dbl(c, 1, 1));
    if r.is_nan() || r < 0.0 {
        return E_INVALIDARG;
    }
    engine(b, |e| {
        if e.st.rate != r {
            e.st.rate = r;
            e.st.event(ev::RATECHANGE);
        }
    });
    timeline(b, |tl, t| tl.set_rate(t, r));
    0
}
fn en_play(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let Some((rewind, run)) = engine(b, |e| {
        let r = e.st.play();
        if r.0 {
            e.ticked = None;
        }
        r
    }) else {
        return 0;
    };
    timeline(b, |tl, t| {
        if rewind {
            tl.seek(t, 0);
        }
        if run {
            tl.run(t);
        }
    });
    0
}
fn en_pause(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    engine(b, |e| e.st.pause());
    timeline(b, |tl, t| tl.pause(t));
    0
}
fn en_volume(c: &mut Ctx) -> u32 {
    let v = engine(c.a(0), |e| e.st.volume).unwrap_or(1.0);
    ret_f64(c, v)
}
fn en_set_volume(c: &mut Ctx) -> u32 {
    let (b, v) = (c.a(0), dbl(c, 1, 1));
    if v.is_nan() {
        return E_INVALIDARG;
    }
    let v = v.clamp(0.0, 1.0);
    engine(b, |e| {
        if e.st.volume != v {
            e.st.volume = v;
            e.st.event(ev::VOLUMECHANGE);
        }
    });
    apply_volume(b);
    0
}
fn en_balance(c: &mut Ctx) -> u32 {
    ret_f64(c, 0.0)
}
/// GetNativeVideoSize(DWORD *cx, DWORD *cy) / GetVideoAspectRatio (the picture's, reduced)
fn video_size(c: &mut Ctx, aspect: bool) -> u32 {
    let (cx, cy) = (c.a(1), c.a(2));
    let Some((w, h)) = engine(c.a(0), |e| e.st.video).flatten() else {
        // (Windows clears the sizes when there is no video)
        for p in [cx, cy] {
            if p != 0 {
                put32(p, 0);
            }
        }
        return E_FAIL;
    };
    let (w, h) = if aspect {
        let g = gcd(w, h).max(1);
        (w / g, h / g)
    } else {
        (w, h)
    };
    put32(cx, w);
    put32(cy, h);
    0
}
fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}
fn en_native_size(c: &mut Ctx) -> u32 {
    video_size(c, false)
}
fn en_aspect(c: &mut Ctx) -> u32 {
    video_size(c, true)
}
/// OnVideoStreamTick(LONGLONG *pts): S_OK and the frame's time when there is a frame not told
/// of yet, else S_FALSE
fn en_tick(c: &mut Ctx) -> u32 {
    let (b, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let Some((pos, rate, frames, ..)) = position(b) else { return S_FALSE };
    let n = frame_at(pos, rate, frames);
    if engine(b, |e| e.ticked.replace(n) == Some(n)).unwrap_or(true) {
        return S_FALSE;
    }
    let pts = n as u128 * rate.1 as u128 * SEC as u128 / rate.0.max(1) as u128;
    w64(out, pts as u64);
    0
}

/// The pixel rectangle (l, t, w, h) of normalized source rectangle `p` (NULL: all) of a w x h
/// picture.
fn src_rect(p: u32, w: u32, h: u32) -> play::R {
    if p == 0 {
        return (0, 0, w as i32, h as i32);
    }
    let f = |i: u32| f32::from_bits(r32(p + 4 * i)).clamp(0.0, 1.0);
    let (l, t, r, bt) = ((f(0) * w as f32) as i32, (f(1) * h as f32) as i32, (f(2) * w as f32).round() as i32, (f(3) * h as f32).round() as i32);
    (l, t, (r - l).max(0), (bt - t).max(0))
}

/// The frame shown now, letterboxed onto `dst` (w x h) with `border` around it: rows of
/// B, G, R, A pixels (`rgba`: R, G, B, A).
pub fn compose(frame: &[u32], fw: u32, fh: u32, src: play::R, dst: (u32, u32), border: u32, rgba: bool) -> Vec<u32> {
    let mut f = Frames::default();
    f.compose(frame, fw, fh, src, dst, border, rgba);
    f.out
}

/// Pixels as TransferVideoFrame hands them out: opaque, R and B swapped for an RGBA texture.
fn texel(p: u32, rgba: bool) -> u32 {
    let p = p | 0xff00_0000;
    if rgba { (p & 0xff00_ff00) | (p & 0xff) << 16 | (p >> 16) & 0xff } else { p }
}

/// What TransferVideoFrame keeps of an engine from one call to the next (no allocation per
/// frame): the picture decoded last (kura::video leaves the old pixels when no newer frame came
/// in time, so that one stays), the frame as written and the letterboxed part of it.
#[derive(Default)]
struct Frames {
    px: Vec<u32>,
    /// the movie size `px` holds a picture of (None: none yet)
    size: Option<(u32, u32)>,
    out: Vec<u32>,
    part: Vec<u32>,
}

impl Frames {
    /// The picture (fw x fh, `frame`) letterboxed onto `dst` with `border` around it into `out`.
    fn compose(&mut self, frame: &[u32], fw: u32, fh: u32, src: play::R, dst: (u32, u32), border: u32, rgba: bool) {
        let (dw, dh) = (dst.0 as i32, dst.1 as i32);
        let out = &mut self.out;
        out.clear();
        out.resize((dw * dh) as usize, border);
        let lb = letterbox((0, 0, dw, dh), src.2, src.3);
        if lb.2 > 0 && lb.3 > 0 {
            if src == (0, 0, fw as i32, fh as i32) && lb == src && (dw, dh) == (lb.2, lb.3) && frame.len() >= out.len() {
                // the whole picture at its size: no scaling, straight across
                for (d, &p) in out.iter_mut().zip(frame) {
                    *d = texel(p, rgba);
                }
                return;
            }
            let part = &mut self.part;
            part.clear();
            part.resize((lb.2 * lb.3) as usize, 0);
            play::blit(frame, fw as i32, fh as i32, src, part, lb, lb);
            for y in 0..lb.3 {
                let o = ((lb.1 + y) * dw + lb.0) as usize;
                out[o..o + lb.2 as usize].copy_from_slice(&part[(y * lb.2) as usize..((y + 1) * lb.2) as usize]);
            }
        }
        if rgba {
            for p in out.iter_mut() {
                *p = (*p & 0xff00_ff00) | (*p & 0xff) << 16 | (*p >> 16) & 0xff;
            }
        }
    }
}

/// How many times TransferVideoFrame asks for the first frame of a movie while it is not there
/// yet, 5 ms apart (each ask waits a little in kura::video too); once there was a picture, the
/// old one is shown instead after `LATE_TRIES` (the game's render thread is not held up for a
/// slow decoder: it draws the last frame again, as Windows' does when the next is late).
const FRAME_TRIES: u32 = 100;
const LATE_TRIES: u32 = 1;
/// The largest movie picture taken (16384 x 16384: D3D11's largest texture).
const MAX_PIXELS: u64 = 16384 * 16384;

/// The pixels of a w x h picture, or None when it is no picture (0, or past `MAX_PIXELS`).
fn picture_pixels(w: u32, h: u32) -> Option<usize> {
    let n = w as u64 * h as u64;
    (n > 0 && n <= MAX_PIXELS).then_some(n as usize)
}

/// TransferVideoFrame(IUnknown *texture, MFVideoNormalizedRect *src, RECT *dst, MFARGB *border):
/// the frame shown now into the game's D3D11 texture
fn en_transfer(c: &mut Ctx) -> u32 {
    let (b, surf, src, dst, border) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if surf == 0 {
        return E_POINTER;
    }
    let Some((pos, rate, frames, stream, w, h)) = position(b) else { return NO_FRAME_TO_TRANSFER };
    let Some((tw, th, fmt)) = kura::d3d11::texture_desc(surf) else { return E_INVALIDARG };
    let rgba = match fmt {
        87 | 88 | 91 | 93 => false, // B8G8R8A8 / X8 (UNORM, SRGB)
        28 | 29 => true,            // R8G8B8A8
        _ => return MF_E_INVALIDMEDIATYPE,
    };
    let (l, t, r, bt) = if dst == 0 { (0, 0, tw as i32, th as i32) } else { (r32(dst) as i32, r32(dst + 4) as i32, r32(dst + 8) as i32, r32(dst + 12) as i32) };
    let (l, t, r, bt) = (l.max(0), t.max(0), r.min(tw as i32), bt.min(th as i32));
    if r <= l || bt <= t {
        return E_INVALIDARG;
    }
    let Some(pixels) = picture_pixels(w, h) else { return E_FAIL };
    let n = frame_at(pos, rate, frames);
    // (taken out of the engine while it decodes: the events thread goes on)
    let Some(mut f) = engine(b, |e| std::mem::take(&mut e.frames)) else { return NO_FRAME_TO_TRANSFER };
    if f.size != Some((w, h)) {
        f.size = None;
        f.px.clear();
        f.px.resize(pixels, 0);
    }
    // (the page may not have the frame's bytes or picture yet: the movie thread draws it a turn
    // later, the game wants it now)
    let mut tries = 0;
    let mut shown = true;
    while !unsafe { play::frame(stream, n as i32, f.px.as_mut_ptr() as *mut u8, 4 * w as usize, w, h) } {
        tries += 1;
        if play::failed(stream) || tries > if f.size.is_some() { LATE_TRIES } else { FRAME_TRIES } {
            shown = false;
            break;
        }
        kura::snap::sleep(std::time::Duration::from_millis(5));
    }
    let done = if !shown && f.size.is_none() {
        E_FAIL
    } else {
        f.size = Some((w, h));
        let border = if border == 0 { 0xff00_0000 } else { r32(border) };
        let px = std::mem::take(&mut f.px);
        f.compose(&px, w, h, src_rect(src, w, h), ((r - l) as u32, (bt - t) as u32), border, rgba);
        f.px = px;
        let done = unsafe { kura::d3d11::write_texture_from(surf, f.out.as_ptr() as *const u8, 4 * (r - l) as u32, [l as u32, t as u32, r as u32, bt as u32]) };
        if done { 0 } else { E_INVALIDARG }
    };
    engine(b, |e| e.frames = f);
    done
}
/// GetResourceCharacteristics(DWORD *): it can seek and pause
fn en_characteristics(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    if engine(c.a(0), |e| e.movie) != Some(true) {
        return MF_E_INVALIDREQUEST;
    }
    put32(c.a(1), 0x2 | 0x4);
    0
}
fn en_streams(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let n = engine(c.a(0), |e| e.st.video.is_some() as u32 + e.st.audio as u32).unwrap_or(0);
    put32(c.a(1), n);
    0
}
fn en_false_out(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put32(c.a(1), 0);
    0
}
fn en_true(_: &mut Ctx) -> u32 {
    1
}
fn en_get_category(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put32(c.a(1), engine(c.a(0), |e| e.category).unwrap_or(0));
    0
}
fn en_set_category(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    engine(c.a(0), |e| e.category = v);
    0
}
fn en_get_role(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put32(c.a(1), engine(c.a(0), |e| e.role).unwrap_or(0));
    0
}
fn en_set_role(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    engine(c.a(0), |e| e.role = v);
    0
}
/// CanPlayType(BSTR type, MF_MEDIA_ENGINE_CANPLAY *answer): "maybe" for video and audio
fn en_can_play(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 {
        return E_POINTER;
    }
    let t = if c.a(1) == 0 { String::new() } else { String::from_utf16_lossy(&mem::wstr(c.a(1))) };
    put32(c.a(2), (t.starts_with("video/") || t.starts_with("audio/")) as u32);
    0
}

/// After Shutdown, the methods that answer an HRESULT answer MF_E_SHUTDOWN.
macro_rules! hr {
    ($f:expr) => {{
        fn w(c: &mut Ctx) -> u32 {
            if shut(c.a(0)) {
                return MF_E_SHUTDOWN;
            }
            ($f)(c)
        }
        w as Handler
    }};
}

/// IMFMediaEngine's 45 methods and IMFMediaEngineEx's 37 (x86 argument bytes)
fn engine_methods() -> Vec<(Handler, u32)> {
    vec![
        (en_qi as Handler, 12),
        (en_add_ref, 4),
        (en_release, 4),
        (hr!(en_get_error), 8),
        (hr!(en_set_error), 8),
        (hr!(notimpl), 8), // SetSourceElements
        (hr!(en_set_source), 8),
        (hr!(en_get_source), 8),
        (en_network, 4),
        (zero, 4),       // GetPreload
        (hr!(ok), 8),    // SetPreload
        (hr!(notimpl), 8), // GetBuffered
        (hr!(en_load), 4),
        (hr!(en_can_play), 12),
        (en_ready, 4),
        (en_seeking, 4),
        (en_time, 4),
        (hr!(en_set_time), 12),
        (en_start_time, 4),
        (en_duration, 4),
        (en_paused, 4),
        (en_default_rate, 4),
        (hr!(en_set_default_rate), 12),
        (en_rate, 4),
        (hr!(en_set_rate), 12),
        (hr!(notimpl), 8), // GetPlayed
        (hr!(notimpl), 8), // GetSeekable
        (en_ended, 4),
        (en_get_autoplay, 4),
        (hr!(en_set_autoplay), 8),
        (en_get_loop, 4),
        (hr!(en_set_loop), 8),
        (hr!(en_play), 4),
        (hr!(en_pause), 4),
        (en_get_muted, 4),
        (hr!(en_set_muted), 8),
        (en_volume, 4),
        (hr!(en_set_volume), 12),
        (en_has_video, 4),
        (en_has_audio, 4),
        (hr!(en_native_size), 12),
        (hr!(en_aspect), 12),
        (hr!(en_shutdown), 4),
        (hr!(en_transfer), 20),
        (hr!(en_tick), 8),
        // IMFMediaEngineEx
        (hr!(notimpl), 12), // SetSourceFromByteStream
        (hr!(notimpl), 12), // GetStatistics
        (hr!(ok), 16),      // UpdateVideoStream
        (en_balance, 4),
        (hr!(ok), 12), // SetBalance
        (en_true, 12), // IsPlaybackRateSupported
        (hr!(notimpl), 8), // FrameStep
        (hr!(en_characteristics), 8),
        (hr!(notimpl), 12), // GetPresentationAttribute
        (hr!(en_streams), 8),
        (hr!(notimpl), 16), // GetStreamAttribute
        (hr!(notimpl), 12), // GetStreamSelection
        (hr!(ok), 12),      // SetStreamSelection
        (hr!(ok), 4),       // ApplyStreamSelections
        (hr!(en_false_out), 8), // IsProtected
        (hr!(notimpl), 12), // InsertVideoEffect
        (hr!(notimpl), 12), // InsertAudioEffect
        (hr!(ok), 4),       // RemoveAllEffects
        (hr!(ok), 12),      // SetTimelineMarkerTimer
        (hr!(notimpl), 8),  // GetTimelineMarkerTimer
        (hr!(ok), 4),       // CancelTimelineMarkerTimer
        (zero, 4),          // IsStereo3D
        (hr!(en_false_out), 8), // GetStereo3DFramePackingMode
        (hr!(ok), 8),       // SetStereo3DFramePackingMode
        (hr!(en_false_out), 8), // GetStereo3DRenderMode
        (hr!(ok), 8),       // SetStereo3DRenderMode
        (hr!(ok), 8),       // EnableWindowlessSwapchainMode
        (hr!(notimpl), 8),  // GetVideoSwapchainHandle
        (hr!(ok), 8),       // EnableHorizontalMirrorMode
        (hr!(en_get_category), 8),
        (hr!(en_set_category), 8),
        (hr!(en_get_role), 8),
        (hr!(en_set_role), 8),
        (hr!(en_false_out), 8), // GetRealTimeMode
        (hr!(ok), 8),       // SetRealTimeMode
        (hr!(en_seek_ex), 16),
        (hr!(ok), 8), // EnableTimeUpdateTimer
    ]
}

// ---------------------------------------------------------------------------------------------
// IMFMediaEngineClassFactory

// MF_MEDIA_ENGINE_CALLBACK {c60381b8-83a4-41f8-a3d0-de05076849a9}
fn is_callback(g: &Guid) -> bool {
    is(g, 0xc603_81b8, 0x83a4, 0x41f8)
}
// MF_MEDIA_ENGINE_AUDIO_CATEGORY {c8d4c51d-350e-41f2-ba46-faebbb0857f6}
fn is_category(g: &Guid) -> bool {
    is(g, 0xc8d4_c51d, 0x350e, 0x41f2)
}
// MF_MEDIA_ENGINE_AUDIO_ENDPOINT_ROLE {d2cb93d1-116a-44f2-9385-f7d0fda2fb46}
fn is_role(g: &Guid) -> bool {
    is(g, 0xd2cb_93d1, 0x116a, 0x44f2)
}

/// CreateInstance(DWORD flags, IMFAttributes *attr, IMFMediaEngine **out): MF_MEDIA_ENGINE_CALLBACK
/// is needed
fn fa_create(c: &mut Ctx) -> u32 {
    let (_flags, attr, out) = (c.a(1), c.a(2), c.a(3));
    if out == 0 {
        return E_POINTER;
    }
    putp(out, 0);
    if attr == 0 {
        return E_POINTER;
    }
    let Some(s) = store_of(attr) else { return E_INVALIDARG };
    let notify = s.items.iter().find_map(|(k, v)| match v {
        Val::Unk(u) if is_callback(k) && *u != 0 => Some(*u),
        _ => None,
    });
    let Some(notify) = notify else { return MF_E_ATTRIBUTENOTFOUND };
    let u32_of = |f: fn(&Guid) -> bool| s.items.iter().find_map(|(k, v)| if let (true, Val::U32(x)) = (f(k), v) { Some(*x) } else { None });
    let (category, role) = (u32_of(is_category).unwrap_or(0), u32_of(is_role).unwrap_or(0));
    add_ref(c, notify);
    static VT: OnceLock<u32> = OnceLock::new();
    let b = object(*VT.get_or_init(|| vtable("IMFMediaEngine", &engine_methods())), 1);
    let e = Engine { refs: 1, st: State::default(), notify, shut: false, src: None, movie: false, ticked: None, category, role, thread: false, frames: Frames::default() };
    ENGINES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, e);
    start_events(b);
    putp(out, b);
    0
}
fn fa_qi(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let g = guid_at(c.a(1));
    // IMFMediaEngineClassFactory {4d645ace-26aa-4688-9be1-df3516990b93}
    if is_unknown(&g) || is(&g, 0x4d64_5ace, 0x26aa, 0x4688) {
        putp(out, this);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn fa_create_error(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    putp(c.a(1), new_error(0, 0));
    0
}
fn one(_: &mut Ctx) -> u32 {
    1
}

/// CoCreateInstance(CLSID_MFMediaEngineClassFactory {b44392da-499b-446b-a4cb-005fead0e6d5}, ...)
pub fn create_instance(clsid: u32, iid: u32) -> Option<u32> {
    let g = guid_at(clsid);
    if !is(&g, 0xb443_92da, 0x499b, 0x446b) {
        return None;
    }
    let i = guid_at(iid);
    if !is_unknown(&i) && !is(&i, 0x4d64_5ace, 0x26aa, 0x4688) {
        return Some(0);
    }
    static F: OnceLock<u32> = OnceLock::new();
    Some(*F.get_or_init(|| {
        let vt = vtable("IMFMediaEngineClassFactory", &[(fa_qi as Handler, 12), (one, 4), (one, 4), (fa_create, 16), (notimpl, 8), (fa_create_error, 8)]);
        object(vt, 1)
    }))
}

// ---------------------------------------------------------------------------------------------

fn MFStartup(_: &mut Ctx) -> u32 {
    0
}
fn MFShutdown(_: &mut Ctx) -> u32 {
    0
}
/// out pointer in argument `i`: cleared
fn none(c: &mut Ctx, i: u32) -> u32 {
    let p = c.a(i);
    if p != 0 {
        putp(p, 0);
    }
    E_NOTIMPL
}

pub fn init() {
    implement("mfplat", "MFStartup", MFStartup);
    implement("mfplat", "MFShutdown", MFShutdown);
    implement("mfplat", "MFCreateAttributes", MFCreateAttributes);
    implement("mfplat", "MFCreateDXGIDeviceManager", MFCreateDXGIDeviceManager);
    session::init();
    topology::init();
    dxgi::init();
    reader::init();
    plat::init();
    source::init();
}

mod mp4;
pub mod movie;
pub mod session;
mod topology;
mod dxgi;
mod source;
mod vstream;
mod plat;
mod reader;

#[cfg(test)]
mod tests;
