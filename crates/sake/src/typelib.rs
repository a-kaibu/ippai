//! oleaut32 type libraries: LoadTypeLib reads MSFT-format libraries (a TYPELIB resource or a
//! .tlb file) and hands out ITypeLib / ITypeInfo objects, enough for a host that implements
//! IDispatch through its type information (ITypeInfo::GetIDsOfNames and Invoke, as
//! CreateStdDispatch or ATL's IDispatchImpl do): Invoke converts the DISPPARAMS' VARIANTs to
//! the method's parameter types and calls the interface's vtable slot.
//! Also the VARIANT reading and writing other modules share (jscript.rs).

#![allow(non_snake_case)]

use crate::api::{implement, vtable, Ctx};
use crate::mem::{self, put32, r16, r32, w16, w32};
use std::sync::Mutex;

const S_OK: u32 = 0;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_INVALIDARG: u32 = 0x8007_0057;
const DISP_E_MEMBERNOTFOUND: u32 = 0x8002_0003;
const DISP_E_PARAMNOTFOUND: u32 = 0x8002_0004;
const DISP_E_TYPEMISMATCH: u32 = 0x8002_0005;
const DISP_E_UNKNOWNNAME: u32 = 0x8002_0006;
const DISP_E_EXCEPTION: u32 = 0x8002_0009;
const DISP_E_BADPARAMCOUNT: u32 = 0x8002_000e;
const TYPE_E_ELEMENTNOTFOUND: u32 = 0x8002_802b;
const TYPE_E_CANTLOADLIBRARY: u32 = 0x8002_9c4a;

pub const VT_EMPTY: u16 = 0;
pub const VT_NULL: u16 = 1;
pub const VT_I2: u16 = 2;
pub const VT_I4: u16 = 3;
pub const VT_R4: u16 = 4;
pub const VT_R8: u16 = 5;
pub const VT_BSTR: u16 = 8;
pub const VT_DISPATCH: u16 = 9;
pub const VT_ERROR: u16 = 10;
pub const VT_BOOL: u16 = 11;
pub const VT_VARIANT: u16 = 12;
pub const VT_UNKNOWN: u16 = 13;
pub const VT_BYREF: u16 = 0x4000;

// ---------------------------------------------------------------------------------------------
// VARIANT

/// A VARIANT's value, as far as scripts and type-info calls need.
#[derive(Clone, Debug, PartialEq)]
pub enum Var {
    Empty,
    Null,
    I4(i32),
    R8(f64),
    Bool(bool),
    Str(Vec<u16>),
    /// VT_DISPATCH (or VT_UNKNOWN when the flag is false)
    Obj(u32, bool),
    /// VT_ERROR (DISP_E_PARAMNOTFOUND for a missing optional argument)
    Error(u32),
}

/// Reads the VARIANT at `p` (through VT_BYREF).
pub fn read_var(p: u32) -> Var {
    let vt = r16(p);
    let (base, at) = if vt & VT_BYREF != 0 {
        (vt & !VT_BYREF, r32(p + 8))
    } else {
        (vt, p + 8)
    };
    if vt & VT_BYREF != 0 && base == VT_VARIANT {
        return read_var(at);
    }
    let f64_at = |a: u32| f64::from_bits(r32(a) as u64 | (r32(a + 4) as u64) << 32);
    match base {
        VT_EMPTY => Var::Empty,
        VT_NULL => Var::Null,
        VT_I2 => Var::I4(r16(at) as i16 as i32),
        VT_I4 | 22 => Var::I4(r32(at) as i32),
        17 => Var::I4(mem::r8(at) as i32),
        16 => Var::I4(mem::r8(at) as i8 as i32),
        18 => Var::I4(r16(at) as i32),
        19 | 23 => {
            let v = r32(at);
            if v <= i32::MAX as u32 {
                Var::I4(v as i32)
            } else {
                Var::R8(v as f64)
            }
        }
        20 | 21 => Var::R8(f64_at(at)),
        VT_R4 => Var::R8(f32::from_bits(r32(at)) as f64),
        VT_R8 | 7 => Var::R8(f64_at(at)),
        6 => Var::R8((r32(at) as u64 | (r32(at + 4) as u64) << 32) as i64 as f64 / 10000.),
        VT_BOOL => Var::Bool(r16(at) != 0),
        VT_ERROR => Var::Error(r32(at)),
        VT_BSTR => {
            let b = r32(at);
            Var::Str(if b == 0 {
                Vec::new()
            } else {
                (0..r32(b - 4) / 2).map(|i| r16(b + 2 * i)).collect()
            })
        }
        VT_DISPATCH => Var::Obj(r32(at), true),
        VT_UNKNOWN => Var::Obj(r32(at), false),
        _ => Var::Empty,
    }
}

/// Writes `v` to the VARIANT at `p` (a new BSTR for a string; objects are not AddRef'd).
pub fn write_var(p: u32, v: &Var) {
    mem::fill(p, 16, 0);
    let (vt, lo, hi) = match v {
        Var::Empty => (VT_EMPTY, 0, 0),
        Var::Null => (VT_NULL, 0, 0),
        Var::I4(i) => (VT_I4, *i as u32, 0),
        Var::R8(f) => (VT_R8, f.to_bits() as u32, (f.to_bits() >> 32) as u32),
        Var::Bool(b) => (VT_BOOL, if *b { 0xffff } else { 0 }, 0),
        Var::Str(s) => (VT_BSTR, crate::misc::bstr_alloc(s), 0),
        Var::Obj(o, d) => (if *d { VT_DISPATCH } else { VT_UNKNOWN }, *o, 0),
        Var::Error(e) => (VT_ERROR, *e, 0),
    };
    w16(p, vt);
    w32(p + 8, lo);
    w32(p + 12, hi);
}

impl Var {
    pub fn num(&self) -> f64 {
        match self {
            Var::I4(i) => *i as f64,
            Var::R8(f) => *f,
            Var::Bool(b) => -(*b as i32) as f64,
            Var::Str(s) => String::from_utf16_lossy(s).trim().parse().unwrap_or(0.),
            _ => 0.,
        }
    }
    fn int(&self) -> i32 {
        match self {
            Var::I4(i) => *i,
            Var::Error(e) => *e as i32,
            v => {
                let f = v.num().round();
                if f >= i32::MIN as f64 && f <= u32::MAX as f64 {
                    f as i64 as u32 as i32
                } else {
                    0
                }
            }
        }
    }
    fn text(&self) -> Vec<u16> {
        match self {
            Var::Str(s) => s.clone(),
            Var::I4(i) => i.to_string().encode_utf16().collect(),
            Var::R8(f) => f.to_string().encode_utf16().collect(),
            Var::Bool(b) => (if *b { "True" } else { "False" }).encode_utf16().collect(),
            _ => Vec::new(),
        }
    }
    fn truthy(&self) -> bool {
        match self {
            Var::Bool(b) => *b,
            Var::Str(s) => !s.is_empty(),
            v => v.num() != 0.,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// the MSFT format

#[derive(Clone, Debug, PartialEq)]
pub enum Td {
    Base(u16),
    Ptr(Box<Td>),
    User(u32),
    Other(u16),
}

#[derive(Clone, Debug)]
pub struct Param {
    pub td: Td,
    pub flags: u16,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Func {
    pub memid: i32,
    pub name: String,
    pub funckind: u32,
    pub invkind: u32,
    pub callconv: u32,
    pub ovft: u32,
    pub ret: Td,
    pub params: Vec<Param>,
    pub nopt: u16,
    pub flags: u16,
}

#[derive(Clone, Debug)]
pub struct Var_ {
    pub memid: i32,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Info {
    pub kind: u32,
    pub flags: u32,
    pub guid: [u8; 16],
    pub name: String,
    pub funcs: Vec<Func>,
    pub vars: Vec<Var_>,
    /// href of the base interface / implemented interfaces
    pub impls: Vec<u32>,
    pub cb_vft: u16,
    pub size: u32,
    pub align: u16,
    pub version: u32,
    pub alias: Option<Td>,
}

#[derive(Clone, Debug)]
pub struct Lib {
    pub name: String,
    pub guid: [u8; 16],
    pub lcid: u32,
    pub version: u32,
    pub flags: u32,
    pub infos: Vec<Info>,
}

/// Parses an MSFT type library.
pub fn parse(d: &[u8]) -> Option<Lib> {
    let i32_at =
        |o: usize| -> Option<i32> { Some(i32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?)) };
    let i16_at =
        |o: usize| -> Option<i16> { Some(i16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?)) };
    if d.get(..4)? != b"MSFT" {
        return None;
    }
    let n = i32_at(0x20)?.max(0) as usize;
    let varflags = i32_at(0x14)?;
    let so = 0x54 + 4 * n + if varflags & 0x100 != 0 { 4 } else { 0 };
    let seg = |k: usize| -> Option<usize> { Some(i32_at(so + 16 * k)?.max(0) as usize) };
    let (ti_tab, guid_tab, name_tab, td_tab) = (seg(0)?, seg(5)?, seg(7)?, seg(9)?);
    let name = |off: i32| -> String {
        if off < 0 {
            return String::new();
        }
        let o = name_tab + off as usize;
        let len = *d.get(o + 8).unwrap_or(&0) as usize;
        d.get(o + 12..o + 12 + len)
            .map(|b| b.iter().map(|&c| c as char).collect())
            .unwrap_or_default()
    };
    let guid = |off: i32| -> [u8; 16] {
        if off < 0 {
            return [0; 16];
        }
        let o = guid_tab + off as usize;
        d.get(o..o + 16)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 16])
    };
    fn tdesc(d: &[u8], td_tab: usize, t: i32, depth: u32) -> Td {
        if t < 0 {
            return Td::Base((t & 0xfff) as u16);
        }
        let o = td_tab + t as usize;
        let h = |k: usize| {
            d.get(o + 2 * k..o + 2 * k + 2)
                .map_or(0, |b| i16::from_le_bytes([b[0], b[1]]))
        };
        let (vt, a, b) = ((h(0) & 0xfff) as u16, h(2), h(3));
        let r = ((b as i32) << 16) | (a as u16 as i32);
        match vt {
            26 | 27 if depth < 16 => {
                let inner = if b < 0 {
                    Td::Base((a & 0xfff) as u16)
                } else {
                    tdesc(d, td_tab, r, depth + 1)
                };
                if vt == 26 {
                    Td::Ptr(Box::new(inner))
                } else {
                    Td::Other(27)
                }
            }
            29 => Td::User(r as u32),
            v => Td::Other(v),
        }
    }
    let td = |t: i32| tdesc(d, td_tab, t, 0);
    let mut infos = Vec::new();
    for i in 0..n {
        let b = ti_tab + i * 100;
        let typekind = i32_at(b)?;
        let mo = i32_at(b + 4)?;
        let ce = i32_at(b + 0x18)?;
        let (nf, nv) = ((ce & 0xffff) as usize, (ce >> 16 & 0xffff) as usize);
        let kind = (typekind & 0xf) as u32;
        let mut info = Info {
            kind,
            flags: i32_at(b + 0x30)? as u32,
            guid: guid(i32_at(b + 0x2c)?),
            name: name(i32_at(b + 0x34)?),
            funcs: Vec::new(),
            vars: Vec::new(),
            impls: Vec::new(),
            cb_vft: i16_at(b + 0x4e)? as u16,
            size: i32_at(b + 0x50)? as u32,
            align: ((typekind >> 11) & 0x1f) as u16,
            version: i32_at(b + 0x38)? as u32,
            alias: None,
        };
        let dt1 = i32_at(b + 0x54)?;
        match kind {
            6 => info.alias = Some(td(dt1)),
            5 => {
                // coclass: dt1 is an offset in the reference table (href, flags, ?, next)
                let rt = seg(3)?;
                let mut at = dt1;
                for _ in 0..i16_at(b + 0x4c)?.max(0) {
                    if at < 0 {
                        break;
                    }
                    info.impls.push(i32_at(rt + at as usize)? as u32);
                    at = i32_at(rt + at as usize + 12)?;
                }
            }
            _ => {
                if dt1 != -1 {
                    info.impls.push(dt1 as u32);
                }
            }
        }
        if mo >= 0 && (nf > 0 || nv > 0) && (mo as usize) < d.len() {
            let mo = mo as usize;
            let infolen = i32_at(mo)?.max(0) as usize;
            let tail = mo.saturating_add(infolen);
            let mut ro = mo + 4;
            let mut prev_name = String::new();
            for f in 0..nf {
                let rl = (i32_at(ro)? & 0xffff) as usize;
                let fk = i32_at(ro + 16)? as u32;
                let na = i16_at(ro + 20)?.max(0) as usize;
                let invkind = fk >> 3 & 0xf;
                let no = i32_at(tail + 4 * (nf + nv + f + 1))?;
                let fname = if no == -1 && f > 0 {
                    prev_name.clone()
                } else {
                    name(no)
                };
                prev_name = fname.clone();
                let mut params = Vec::new();
                let pbase = (ro + rl).checked_sub(na * 12)?;
                for j in 0..na {
                    let p = pbase + j * 12;
                    params.push(Param {
                        td: td(i32_at(p)?),
                        name: name(i32_at(p + 4)?),
                        flags: i32_at(p + 8)? as u16,
                    });
                }
                info.funcs.push(Func {
                    memid: i32_at(tail + 4 * (f + 1))?,
                    name: fname,
                    funckind: fk & 7,
                    invkind,
                    callconv: fk >> 8 & 0xf,
                    ovft: (i16_at(ro + 12)? as u16 & !1) as u32,
                    ret: td(i32_at(ro + 4)?),
                    params,
                    nopt: i16_at(ro + 22)? as u16,
                    flags: i32_at(ro + 8)? as u16,
                });
                ro += rl.max(4);
            }
            for v in 0..nv {
                info.vars.push(Var_ {
                    memid: i32_at(tail + 4 * (nf + v + 1))?,
                    name: name(i32_at(tail + 4 * (2 * nf + nv + v + 1))?),
                });
            }
        }
        infos.push(info);
    }
    Some(Lib {
        name: name(i32_at(0x38)?),
        guid: guid(i32_at(0x08)?),
        lcid: i32_at(0x0c)? as u32,
        version: i32_at(0x18)? as u32,
        flags: i32_at(0x1c)? as u32,
        infos,
    })
}

// ---------------------------------------------------------------------------------------------
// objects

struct Loaded {
    lib: Lib,
    block: u32,
    infos: Vec<u32>,
}

static LIBS: Mutex<Vec<Loaded>> = Mutex::new(Vec::new());

fn lib_vtbl() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "ITypeLib",
            &[
                (L_QueryInterface, 12),
                (one, 4),
                (one, 4),
                (L_GetTypeInfoCount, 4),
                (L_GetTypeInfo, 12),
                (L_GetTypeInfoType, 12),
                (L_GetTypeInfoOfGuid, 12),
                (L_GetLibAttr, 8),
                (notimpl, 8),
                (L_GetDocumentation, 24),
                (L_IsName, 16),
                (notimpl, 24),
                (free_it, 8),
            ],
        )
    })
}

fn info_vtbl() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "ITypeInfo",
            &[
                (I_QueryInterface, 12),
                (one, 4),
                (one, 4),
                (I_GetTypeAttr, 8),
                (notimpl, 8),
                (I_GetFuncDesc, 12),
                (notimpl, 12),
                (I_GetNames, 20),
                (I_GetRefTypeOfImplType, 12),
                (I_GetImplTypeFlags, 12),
                (I_GetIDsOfNames, 16),
                (I_Invoke, 32),
                (I_GetDocumentation, 24),
                (notimpl, 24),
                (I_GetRefTypeInfo, 12),
                (notimpl, 16),
                (notimpl, 16),
                (notimpl, 12),
                (I_GetContainingTypeLib, 12),
                (free_it, 8),
                (free_func_desc, 8),
                (free_it, 8),
            ],
        )
    })
}

fn one(_: &mut Ctx) -> u32 {
    1
}
fn notimpl(_: &mut Ctx) -> u32 {
    E_NOTIMPL
}
fn free_it(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 {
        mem::heap_free(c.a(1));
    }
    S_OK
}

/// Registers a parsed library; its ITypeLib.
pub fn add(lib: Lib) -> u32 {
    let mut libs = LIBS.lock().unwrap();
    if let Some(l) = libs
        .iter()
        .find(|l| l.lib.guid == lib.guid && l.lib.guid != [0; 16])
    {
        return l.block;
    }
    let li = libs.len() as u32;
    let block = mem::heap_alloc(8, true);
    w32(block, lib_vtbl());
    w32(block + 4, li);
    let infos = (0..lib.infos.len() as u32)
        .map(|i| {
            let b = mem::heap_alloc(12, true);
            w32(b, info_vtbl());
            w32(b + 4, li);
            w32(b + 8, i);
            b
        })
        .collect();
    libs.push(Loaded { lib, block, infos });
    block
}

fn lib_of(c: &Ctx) -> usize {
    r32(c.a(0) + 4) as usize
}
/// (library, typeinfo index) of the ITypeInfo `c` is called on
fn info_of(c: &Ctx) -> (usize, usize) {
    (r32(c.a(0) + 4) as usize, r32(c.a(0) + 8) as usize)
}
fn with_info<R>(li: usize, ii: usize, f: impl FnOnce(&Lib, &Info) -> R) -> Option<R> {
    let libs = LIBS.lock().unwrap();
    let l = libs.get(li)?;
    l.lib.infos.get(ii).map(|i| f(&l.lib, i))
}
fn info_block(li: usize, ii: usize) -> Option<u32> {
    LIBS.lock()
        .unwrap()
        .get(li)
        .and_then(|l| l.infos.get(ii).copied())
}

fn guid_eq(p: u32, g: &[u8; 16]) -> bool {
    mem::slice(p, 16) == g
}

fn L_QueryInterface(c: &mut Ctx) -> u32 {
    // IUnknown, ITypeLib {00020402-...}, ITypeLib2 {00020411-...}
    match r32(c.a(1)) {
        0 | 0x0002_0402 | 0x0002_0411 => {
            put32(c.a(2), c.a(0));
            S_OK
        }
        _ => {
            put32(c.a(2), 0);
            0x8000_4002
        }
    }
}
fn L_GetTypeInfoCount(c: &mut Ctx) -> u32 {
    LIBS.lock()
        .unwrap()
        .get(lib_of(c))
        .map_or(0, |l| l.lib.infos.len() as u32)
}
fn L_GetTypeInfo(c: &mut Ctx) -> u32 {
    match info_block(lib_of(c), c.a(1) as usize) {
        Some(b) => {
            mem::putp(c.a(2), b);
            S_OK
        }
        None => TYPE_E_ELEMENTNOTFOUND,
    }
}
fn L_GetTypeInfoType(c: &mut Ctx) -> u32 {
    match with_info(lib_of(c), c.a(1) as usize, |_, i| i.kind) {
        Some(k) => {
            put32(c.a(2), k);
            S_OK
        }
        None => TYPE_E_ELEMENTNOTFOUND,
    }
}
fn L_GetTypeInfoOfGuid(c: &mut Ctx) -> u32 {
    let li = lib_of(c);
    let g = c.a(1);
    let hit = LIBS.lock().unwrap().get(li).and_then(|l| {
        l.lib
            .infos
            .iter()
            .position(|i| guid_eq(g, &i.guid))
            .map(|i| l.infos[i])
    });
    match hit {
        Some(b) => {
            mem::putp(c.a(2), b);
            S_OK
        }
        None => {
            put32(c.a(2), 0);
            TYPE_E_ELEMENTNOTFOUND
        }
    }
}
fn L_GetLibAttr(c: &mut Ctx) -> u32 {
    let Some((g, lcid, ver, flags)) = LIBS
        .lock()
        .unwrap()
        .get(lib_of(c))
        .map(|l| (l.lib.guid, l.lib.lcid, l.lib.version, l.lib.flags))
    else {
        return E_INVALIDARG;
    };
    let p = mem::heap_alloc(32, true);
    mem::write(p, &g);
    w32(p + 16, lcid);
    w32(p + 20, 1); // SYS_WIN32
    w16(p + 24, ver as u16);
    w16(p + 26, (ver >> 16) as u16);
    w16(p + 28, flags as u16);
    mem::putp(c.a(1), p);
    S_OK
}
fn put_bstr(p: u32, s: &str) {
    if p != 0 {
        put32(
            p,
            crate::misc::bstr_alloc(&s.encode_utf16().collect::<Vec<_>>()),
        );
    }
}
fn L_GetDocumentation(c: &mut Ctx) -> u32 {
    let li = lib_of(c);
    let idx = c.a(1) as i32;
    let n = LIBS.lock().unwrap().get(li).and_then(|l| {
        if idx < 0 {
            Some(l.lib.name.clone())
        } else {
            l.lib.infos.get(idx as usize).map(|i| i.name.clone())
        }
    });
    let Some(n) = n else {
        return TYPE_E_ELEMENTNOTFOUND;
    };
    put_bstr(c.a(2), &n);
    for p in [c.a(3), c.a(5)] {
        put32(p, 0);
    }
    put32(c.a(4), 0);
    S_OK
}
fn L_IsName(c: &mut Ctx) -> u32 {
    let name = String::from_utf16_lossy(&mem::wstr(c.a(1)));
    let found = LIBS.lock().unwrap().get(lib_of(c)).is_some_and(|l| {
        l.lib.infos.iter().any(|i| {
            i.name.eq_ignore_ascii_case(&name)
                || i.funcs.iter().any(|f| f.name.eq_ignore_ascii_case(&name))
        })
    });
    put32(c.a(3), if found { 0xffff } else { 0 });
    S_OK
}

fn I_QueryInterface(c: &mut Ctx) -> u32 {
    // IUnknown, ITypeInfo {00020401-...}, ITypeInfo2 {00020412-...}
    match r32(c.a(1)) {
        0 | 0x0002_0401 | 0x0002_0412 => {
            put32(c.a(2), c.a(0));
            S_OK
        }
        _ => {
            put32(c.a(2), 0);
            0x8000_4002
        }
    }
}

/// TYPEATTR
fn I_GetTypeAttr(c: &mut Ctx) -> u32 {
    let (li, ii) = info_of(c);
    let Some(i) = with_info(li, ii, |_, i| i.clone()) else {
        return E_INVALIDARG;
    };
    let p = mem::heap_alloc(76, true);
    mem::write(p, &i.guid);
    w32(p + 16, 0);
    w32(p + 24, 0xffff_ffff);
    w32(p + 28, 0xffff_ffff);
    w32(p + 36, i.size);
    w32(p + 40, i.kind);
    w16(p + 44, i.funcs.len() as u16);
    w16(p + 46, i.vars.len() as u16);
    w16(p + 48, i.impls.len() as u16);
    w16(p + 50, i.cb_vft);
    w16(p + 52, i.align);
    w16(p + 54, i.flags as u16);
    w16(p + 56, i.version as u16);
    w16(p + 58, (i.version >> 16) as u16);
    if let Some(a) = &i.alias {
        write_tdesc(p + 60, a);
    }
    mem::putp(c.a(1), p);
    S_OK
}

/// A TYPEDESC (8 bytes) at `p`; pointed-to descriptions are allocated.
fn write_tdesc(p: u32, t: &Td) {
    match t {
        Td::Base(vt) | Td::Other(vt) => {
            w32(p, 0);
            w16(p + 4, *vt);
        }
        Td::Ptr(inner) => {
            let q = mem::heap_alloc(8, true);
            write_tdesc(q, inner);
            w32(p, q);
            w16(p + 4, 26);
        }
        Td::User(h) => {
            w32(p, *h);
            w16(p + 4, 29);
        }
    }
}

/// FUNCDESC (32-bit layout, 52 bytes) with its ELEMDESCs; freed in one piece by
/// ReleaseFuncDesc (the TYPEDESCs pointed to stay: small and few).
fn I_GetFuncDesc(c: &mut Ctx) -> u32 {
    let (li, ii) = info_of(c);
    let Some(f) = with_info(li, ii, |_, i| i.funcs.get(c.a(1) as usize).cloned()).flatten() else {
        return TYPE_E_ELEMENTNOTFOUND;
    };
    let n = f.params.len() as u32;
    let p = mem::heap_alloc(52 + 16 * n, true);
    w32(p, f.memid as u32);
    w32(p + 8, if n > 0 { p + 52 } else { 0 });
    w32(p + 12, f.funckind);
    w32(p + 16, f.invkind);
    w32(p + 20, f.callconv);
    w16(p + 24, n as u16);
    w16(p + 26, f.nopt);
    w16(p + 28, f.ovft as u16);
    write_tdesc(p + 32, &f.ret);
    w16(p + 48, f.flags);
    for (j, a) in f.params.iter().enumerate() {
        let e = p + 52 + 16 * j as u32;
        write_tdesc(e, &a.td);
        w16(e + 12, a.flags);
    }
    mem::putp(c.a(2), p);
    S_OK
}
fn free_func_desc(c: &mut Ctx) -> u32 {
    free_it(c)
}

/// The function named `name` (any case) and the library index of the info it is in, searching
/// base interfaces of this library too.
fn find<R>(li: usize, ii: usize, f: &mut dyn FnMut(usize, &Info) -> Option<R>) -> Option<R> {
    let libs = LIBS.lock().unwrap();
    let l = libs.get(li)?;
    let mut at = ii;
    for _ in 0..16 {
        let info = l.lib.infos.get(at)?;
        if let Some(r) = f(at, info) {
            return Some(r);
        }
        // the base interface, if it is in this library
        match info.impls.first() {
            Some(&h) if h & 1 == 0 && (h as usize) / 100 < l.lib.infos.len() && info.kind != 5 => {
                at = h as usize / 100
            }
            _ => return None,
        }
    }
    None
}

fn I_GetIDsOfNames(c: &mut Ctx) -> u32 {
    let (li, ii) = info_of(c);
    let (names, n, out) = (c.a(1), c.a(2), c.a(3));
    if n == 0 {
        return E_INVALIDARG;
    }
    let first = String::from_utf16_lossy(&mem::wstr(r32(names)));
    let hit = find(li, ii, &mut |_, i| {
        if let Some(f) = i.funcs.iter().find(|f| f.name.eq_ignore_ascii_case(&first)) {
            return Some((
                f.memid,
                f.params.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
            ));
        }
        i.vars
            .iter()
            .find(|v| v.name.eq_ignore_ascii_case(&first))
            .map(|v| (v.memid, Vec::new()))
    });
    let Some((id, params)) = hit else {
        for k in 0..n {
            w32(out + 4 * k, 0xffff_ffff);
        }
        return DISP_E_UNKNOWNNAME;
    };
    w32(out, id as u32);
    let mut r = S_OK;
    for k in 1..n {
        let s = String::from_utf16_lossy(&mem::wstr(r32(names + 4 * k)));
        match params.iter().position(|p| p.eq_ignore_ascii_case(&s)) {
            Some(j) => w32(out + 4 * k, j as u32),
            None => {
                w32(out + 4 * k, 0xffff_ffff);
                r = DISP_E_UNKNOWNNAME;
            }
        }
    }
    r
}

fn I_GetNames(c: &mut Ctx) -> u32 {
    let (li, ii) = info_of(c);
    let (memid, out, max, count) = (c.a(1) as i32, c.a(2), c.a(3), c.a(4));
    let names = find(li, ii, &mut |_, i| {
        if let Some(f) = i.funcs.iter().find(|f| f.memid == memid) {
            let mut v = vec![f.name.clone()];
            v.extend(
                f.params
                    .iter()
                    .filter(|p| p.flags & 8 == 0 || f.invkind & 0xc == 0)
                    .map(|p| p.name.clone()),
            );
            return Some(v);
        }
        i.vars
            .iter()
            .find(|v| v.memid == memid)
            .map(|v| vec![v.name.clone()])
    });
    let Some(names) = names else {
        return TYPE_E_ELEMENTNOTFOUND;
    };
    let k = (names.len() as u32).min(max);
    for (j, n) in names.iter().take(k as usize).enumerate() {
        put_bstr(out + 4 * j as u32, n);
    }
    put32(count, k);
    S_OK
}

fn I_GetDocumentation(c: &mut Ctx) -> u32 {
    let (li, ii) = info_of(c);
    let memid = c.a(1) as i32;
    let n = if memid == -1 {
        with_info(li, ii, |_, i| i.name.clone())
    } else {
        find(li, ii, &mut |_, i| {
            i.funcs
                .iter()
                .find(|f| f.memid == memid)
                .map(|f| f.name.clone())
                .or_else(|| {
                    i.vars
                        .iter()
                        .find(|v| v.memid == memid)
                        .map(|v| v.name.clone())
                })
        })
    };
    let Some(n) = n else {
        return TYPE_E_ELEMENTNOTFOUND;
    };
    put_bstr(c.a(2), &n);
    put32(c.a(3), 0);
    put32(c.a(4), 0);
    put32(c.a(5), 0);
    S_OK
}

fn I_GetRefTypeOfImplType(c: &mut Ctx) -> u32 {
    let (li, ii) = info_of(c);
    let idx = c.a(1) as i32;
    let Some((impls, flags)) = with_info(li, ii, |_, i| (i.impls.clone(), i.flags)) else {
        return E_INVALIDARG;
    };
    if idx == -1 && flags & 0x40 != 0 {
        // a dual interface's vtable side: this info again
        put32(c.a(2), (ii * 100) as u32);
        return S_OK;
    }
    match impls.get(idx.max(0) as usize).filter(|_| idx >= 0) {
        Some(&h) => {
            put32(c.a(2), h);
            S_OK
        }
        None => TYPE_E_ELEMENTNOTFOUND,
    }
}
fn I_GetImplTypeFlags(c: &mut Ctx) -> u32 {
    put32(c.a(2), if c.a(1) == 0 { 1 } else { 0 }); // IMPLTYPEFLAG_FDEFAULT
    S_OK
}
fn I_GetRefTypeInfo(c: &mut Ctx) -> u32 {
    let (li, _) = info_of(c);
    let h = c.a(1);
    if h & 1 != 0 {
        // in another library (stdole2's IDispatch): not loaded
        put32(c.a(2), 0);
        return TYPE_E_CANTLOADLIBRARY;
    }
    match info_block(li, h as usize / 100) {
        Some(b) => {
            mem::putp(c.a(2), b);
            S_OK
        }
        None => TYPE_E_ELEMENTNOTFOUND,
    }
}
fn I_GetContainingTypeLib(c: &mut Ctx) -> u32 {
    let (li, ii) = info_of(c);
    if let Some(b) = LIBS.lock().unwrap().get(li).map(|l| l.block) {
        mem::putp(c.a(1), b);
    }
    put32(c.a(2), ii as u32);
    S_OK
}

// ---------------------------------------------------------------------------------------------
// Invoke

/// What a USERDEFINED type is: (typekind, alias target).
fn user_kind(li: usize, h: u32) -> (u32, Option<Td>) {
    if h & 1 != 0 {
        return (4, None);
    }
    with_info(li, h as usize / 100, |_, i| (i.kind, i.alias.clone())).unwrap_or((4, None))
}

/// The stack dwords for passing `v` as type `t` (temporaries to free after the call).
fn to_arg(
    li: usize,
    t: &Td,
    v: &Var,
    src: u32,
    temps: &mut Vec<u32>,
    bstrs: &mut Vec<u32>,
) -> Result<Vec<u32>, u32> {
    Ok(match t {
        Td::Base(vt) | Td::Other(vt) => match *vt {
            VT_R8 | 7 => {
                let b = v.num().to_bits();
                vec![b as u32, (b >> 32) as u32]
            }
            VT_R4 => vec![(v.num() as f32).to_bits()],
            20 | 21 | 6 => {
                let b = v.num() as i64 as u64;
                vec![b as u32, (b >> 32) as u32]
            }
            VT_BOOL => vec![if v.truthy() { 0xffff_ffff } else { 0 }],
            VT_BSTR => match v {
                Var::Str(_) if src != 0 && r16(src) == VT_BSTR => vec![r32(src + 8)],
                Var::Empty | Var::Null => vec![0],
                _ => {
                    let b = crate::misc::bstr_alloc(&v.text());
                    bstrs.push(b);
                    vec![b]
                }
            },
            VT_DISPATCH | VT_UNKNOWN => match v {
                Var::Obj(o, _) => vec![*o],
                Var::Empty | Var::Null => vec![0],
                Var::I4(0) => vec![0],
                _ => return Err(DISP_E_TYPEMISMATCH),
            },
            VT_VARIANT => {
                let p = mem::heap_alloc(16, true);
                temps.push(p);
                if src != 0 && r16(src) & VT_BYREF == 0 {
                    mem::copy(p, src, 16);
                } else {
                    write_var(p, v);
                    if let Var::Str(_) = v {
                        bstrs.push(r32(p + 8));
                    }
                }
                (0..4).map(|k| r32(p + 4 * k)).collect()
            }
            _ => vec![v.int() as u32],
        },
        Td::User(h) => match user_kind(li, *h) {
            (6, Some(a)) => return to_arg(li, &a, v, src, temps, bstrs),
            (0, _) => vec![v.int() as u32],
            _ => match v {
                Var::Obj(o, _) => vec![*o],
                _ => vec![0],
            },
        },
        Td::Ptr(inner) => {
            // a VARIANT* gets the caller's VARIANT; other pointers a temporary holding the value
            if **inner == Td::Base(VT_VARIANT) && src != 0 {
                return Ok(vec![if r16(src) == VT_BYREF | VT_VARIANT {
                    r32(src + 8)
                } else {
                    src
                }]);
            }
            if src != 0 && r16(src) & VT_BYREF != 0 {
                return Ok(vec![r32(src + 8)]);
            }
            let p = mem::heap_alloc(16, true);
            temps.push(p);
            let d = to_arg(li, inner, v, src, temps, bstrs)?;
            for (k, x) in d.iter().enumerate() {
                w32(p + 4 * k as u32, *x);
            }
            vec![p]
        }
    })
}

/// The value an out parameter of type `t` (pointed to by `p`) holds after the call.
fn from_out(li: usize, t: &Td, p: u32) -> Var {
    match t {
        Td::Base(vt) | Td::Other(vt) => match *vt {
            VT_VARIANT => read_var(p),
            VT_BOOL => Var::Bool(r16(p) != 0),
            VT_BSTR => {
                let b = r32(p);
                Var::Str(if b == 0 {
                    Vec::new()
                } else {
                    (0..r32(b - 4) / 2).map(|i| r16(b + 2 * i)).collect()
                })
            }
            VT_DISPATCH => Var::Obj(r32(p), true),
            VT_UNKNOWN => Var::Obj(r32(p), false),
            VT_R8 | 7 => Var::R8(f64::from_bits(r32(p) as u64 | (r32(p + 4) as u64) << 32)),
            VT_R4 => Var::R8(f32::from_bits(r32(p)) as f64),
            VT_I2 => Var::I4(r16(p) as i16 as i32),
            17 => Var::I4(mem::r8(p) as i32),
            16 => Var::I4(mem::r8(p) as i8 as i32),
            18 => Var::I4(r16(p) as i32),
            19 | 23 => Var::R8(r32(p) as f64),
            _ => Var::I4(r32(p) as i32),
        },
        Td::User(h) => match user_kind(li, *h) {
            (6, Some(a)) => from_out(li, &a, p),
            (0, _) => Var::I4(r32(p) as i32),
            _ => Var::Empty,
        },
        Td::Ptr(inner) => match &**inner {
            Td::User(h) => {
                let (k, _) = user_kind(li, *h);
                Var::Obj(r32(p), k == 4 || k == 3)
            }
            _ => Var::Obj(r32(p), false),
        },
    }
}

/// ITypeInfo::Invoke(pvInstance, memid, wFlags, pDispParams, pVarResult, pExcepInfo, puArgErr)
fn I_Invoke(c: &mut Ctx) -> u32 {
    let (li, ii) = info_of(c);
    let (obj, memid, flags, dp, result, excep) =
        (c.a(1), c.a(2) as i32, c.a(3) & 0xf, c.a(4), c.a(5), c.a(6));
    let Some(f) = find(li, ii, &mut |_, i| {
        i.funcs
            .iter()
            .find(|f| f.memid == memid && f.invkind & flags != 0)
            .cloned()
    }) else {
        return DISP_E_MEMBERNOTFOUND;
    };
    if f.funckind > 1 || obj == 0 {
        return DISP_E_MEMBERNOTFOUND;
    }
    let (rgvarg, named, nargs, nnamed) = if dp == 0 {
        (0, 0, 0, 0)
    } else {
        (r32(dp), r32(dp + 4), r32(dp + 8), r32(dp + 12))
    };
    let npos = nargs.saturating_sub(nnamed);
    let mut args = vec![obj];
    let (mut temps, mut bstrs) = (Vec::new(), Vec::new());
    let mut retval: Option<(u32, Td)> = None;
    let mut k = 0u32; // positional arguments used
    let nin = f.params.iter().filter(|p| p.flags & 0xc == 0).count();
    let mut err = 0;
    for (j, p) in f.params.iter().enumerate() {
        if p.flags & 8 != 0 {
            // FRETVAL
            let t = match &p.td {
                Td::Ptr(inner) => (**inner).clone(),
                t => t.clone(),
            };
            let m = mem::heap_alloc(16, true);
            temps.push(m);
            args.push(m);
            retval = Some((m, t));
            continue;
        }
        if p.flags & 4 != 0 {
            args.push(0x411); // FLCID
            continue;
        }
        // the argument: named by its index (or DISPID_PROPERTYPUT for a put's last), else the
        // next positional one
        let put_last = flags & 0xc != 0 && j + 1 == nin;
        let named_at = (0..nnamed).find(|&n| {
            let id = r32(named + 4 * n) as i32;
            id == j as i32 || (put_last && id == -3)
        });
        let src = match named_at {
            Some(n) => Some(rgvarg + 16 * n),
            None if k < npos => {
                k += 1;
                Some(rgvarg + 16 * (nargs - k))
            }
            None => None,
        };
        let (v, src) = match src {
            Some(s) if !(r16(s) == VT_ERROR && r32(s + 8) == DISP_E_PARAMNOTFOUND) => {
                (read_var(s), s)
            }
            _ if p.flags & 0x30 != 0 => (Var::Error(DISP_E_PARAMNOTFOUND), 0),
            _ => {
                err = DISP_E_BADPARAMCOUNT;
                break;
            }
        };
        let v = if matches!(v, Var::Error(_)) && p.td != Td::Base(VT_VARIANT) && src == 0 {
            Var::Empty
        } else {
            v
        };
        match to_arg(li, &p.td, &v, src, &mut temps, &mut bstrs) {
            Ok(d) => args.extend(d),
            Err(e) => {
                err = e;
                if c.a(7) != 0 {
                    put32(c.a(7), j as u32);
                }
                break;
            }
        }
    }
    let hr = if err != 0 {
        err
    } else {
        let f_addr = r32(r32(obj) + f.ovft);
        let eax = c.call(f_addr, &args);
        let hres = f.ret == Td::Base(25);
        if hres && eax & 0x8000_0000 != 0 {
            if excep != 0 {
                mem::fill(excep, 32, 0);
                w32(excep + 28, eax);
            }
            DISP_E_EXCEPTION
        } else {
            let v = match (&retval, hres) {
                (Some((m, t)), _) => Some(from_out(li, t, *m)),
                (None, false) if f.ret != Td::Base(24) => Some(match &f.ret {
                    Td::Base(VT_BOOL) => Var::Bool(eax & 0xffff != 0),
                    _ => Var::I4(eax as i32),
                }),
                _ => None,
            };
            if result != 0 {
                match v {
                    // the callee's BSTR / object reference becomes the result's
                    Some(Var::Str(_)) if retval.is_some() => {
                        let (m, _) = retval.as_ref().unwrap();
                        mem::fill(result, 16, 0);
                        w16(result, VT_BSTR);
                        w32(result + 8, r32(*m));
                    }
                    Some(Var::Empty) | None => write_var(result, &Var::Empty),
                    Some(v) => write_var(result, &v),
                }
            } else if let Some((m, t)) = &retval {
                if *t == Td::Base(VT_BSTR) {
                    crate::misc::bstr_free(r32(*m));
                }
            }
            S_OK
        }
    };
    for b in bstrs {
        crate::misc::bstr_free(b);
    }
    for t in temps {
        mem::heap_free(t);
    }
    hr
}

// ---------------------------------------------------------------------------------------------
// exports

/// The TYPELIB resource (or the file) `path` names: "x.exe", "x.dll\\2", "x.tlb".
fn load(path: &str) -> Option<Lib> {
    let (file, idx) = match path.rsplit_once('\\') {
        Some((f, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            (f.to_string(), n.parse().unwrap_or(1))
        }
        _ => (path.to_string(), 1),
    };
    if let Some(m) =
        crate::pe::by_name(&file).or_else(|| crate::pe::by_name(&crate::files::full_path(&file)))
    {
        let ty = crate::pe::ResId::Name("TYPELIB".encode_utf16().collect());
        let e = crate::pe::find_resource(m, &ty, &crate::pe::ResId::Int(idx))?;
        let (a, n) = crate::pe::resource_data(m, e);
        return parse(mem::slice(a, n));
    }
    let data = crate::files::resolve(&file).and_then(|h| kura::fs::read(h).ok())?;
    parse(&data)
}

fn load_arg(c: &mut Ctx, out: u32) -> u32 {
    let path = String::from_utf16_lossy(&mem::wstr(c.a(0)));
    match load(&path) {
        Some(l) => {
            put32(out, add(l));
            S_OK
        }
        None => {
            eprintln!("sake: LoadTypeLib {path}: no MSFT type library");
            put32(out, 0);
            TYPE_E_CANTLOADLIBRARY
        }
    }
}
fn LoadTypeLib(c: &mut Ctx) -> u32 {
    load_arg(c, c.a(1))
}
fn LoadTypeLibEx(c: &mut Ctx) -> u32 {
    load_arg(c, c.a(2))
}
/// LoadRegTypeLib(guid, major, minor, lcid, &lib): a library loaded before, by its GUID.
fn LoadRegTypeLib(c: &mut Ctx) -> u32 {
    let g = c.a(0);
    let hit = LIBS
        .lock()
        .unwrap()
        .iter()
        .find(|l| guid_eq(g, &l.lib.guid))
        .map(|l| l.block);
    put32(c.a(4), hit.unwrap_or(0));
    if hit.is_some() {
        S_OK
    } else {
        TYPE_E_CANTLOADLIBRARY
    }
}
fn ok(_: &mut Ctx) -> u32 {
    S_OK
}

pub fn init() {
    implement("oleaut32", "LoadTypeLib", LoadTypeLib);
    implement("oleaut32", "LoadTypeLibEx", LoadTypeLibEx);
    implement("oleaut32", "LoadRegTypeLib", LoadRegTypeLib);
    implement("oleaut32", "RegisterTypeLib", ok);
    implement("oleaut32", "RegisterTypeLibForUser", ok);
    implement("oleaut32", "UnRegisterTypeLib", ok);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A built library: one dual interface "IThing" with getX([out, retval] long*) and
    /// setName(BSTR), deriving from an imported IDispatch.
    fn sample() -> Vec<u8> {
        let mut d = vec![0u8; 0x54 + 4 + 15 * 16];
        let put = |d: &mut Vec<u8>, o: usize, v: i32| d[o..o + 4].copy_from_slice(&v.to_le_bytes());
        d[..4].copy_from_slice(b"MSFT");
        put(&mut d, 0x20, 1);
        put(&mut d, 0x08, 0);
        put(&mut d, 0x38, 0);
        let so = 0x54 + 4;
        // segments: names, guids, typeinfo, typedesc, member data appended after
        let mut names = Vec::new();
        let mut name = |s: &str| -> i32 {
            let o = names.len() as i32;
            names.extend_from_slice(&(-1i32).to_le_bytes());
            names.extend_from_slice(&(-1i32).to_le_bytes());
            names.extend_from_slice(&(s.len() as i32).to_le_bytes());
            names.extend_from_slice(s.as_bytes());
            while names.len() % 4 != 0 {
                names.push(0x57);
            }
            o
        };
        let (n_lib, n_if, n_get, n_set, n_v, n_name) = (
            name("Lib"),
            name("IThing"),
            name("getX"),
            name("setName"),
            name("v"),
            name("name"),
        );
        let guids: Vec<u8> = (0..48u8).collect();
        // typedesc 0: PTR to VT_I4
        let tds: Vec<u8> = [26i16, 0, 3, -1]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        let mut members = Vec::new();
        let rec = |m: &mut Vec<u8>, dt: i32, vft: i16, params: &[(i32, i32, i32)]| {
            let len = 24 + 12 * params.len() as i32;
            for v in [len, dt, 0] {
                m.extend_from_slice(&v.to_le_bytes());
            }
            m.extend_from_slice(&vft.to_le_bytes());
            m.extend_from_slice(&0i16.to_le_bytes());
            m.extend_from_slice(&((1 | 1 << 3 | 4 << 8) as i32).to_le_bytes());
            m.extend_from_slice(&(params.len() as i16).to_le_bytes());
            m.extend_from_slice(&0i16.to_le_bytes());
            for (t, n, f) in params {
                for v in [*t, *n, *f] {
                    m.extend_from_slice(&v.to_le_bytes());
                }
            }
        };
        let mut recs = Vec::new();
        rec(&mut recs, 0x8000_0019u32 as i32, 28, &[(0, n_v, 0xa)]);
        rec(
            &mut recs,
            0x8000_0019u32 as i32,
            32,
            &[(0x8000_0008u32 as i32, n_name, 1)],
        );
        members.extend_from_slice(&(recs.len() as i32).to_le_bytes());
        members.extend_from_slice(&recs);
        for v in [0x6002_0000, 0x6002_0001, n_get, n_set, 0, 0] {
            members.extend_from_slice(&(v as i32).to_le_bytes());
        }
        let mut ti = vec![0u8; 100];
        let tput =
            |t: &mut Vec<u8>, o: usize, v: i32| t[o..o + 4].copy_from_slice(&v.to_le_bytes());
        tput(&mut ti, 0, 4);
        tput(&mut ti, 0x18, 2);
        tput(&mut ti, 0x2c, 16);
        tput(&mut ti, 0x30, 0x1040);
        tput(&mut ti, 0x34, n_if);
        ti[0x4c..0x4e].copy_from_slice(&1i16.to_le_bytes());
        ti[0x4e..0x50].copy_from_slice(&36i16.to_le_bytes());
        tput(&mut ti, 0x54, 1);
        let base = d.len();
        let segs = [
            (0, base),
            (5, base + 100),
            (7, base + 148),
            (9, base + 148 + names.len()),
        ];
        for (k, o) in segs {
            put(&mut d, so + 16 * k, o as i32);
        }
        let mo = base + 148 + names.len() + tds.len();
        tput(&mut ti, 4, mo as i32);
        d.extend_from_slice(&ti);
        d.extend_from_slice(&guids);
        d.extend_from_slice(&names);
        d.extend_from_slice(&tds);
        d.extend_from_slice(&members);
        let _ = n_lib;
        d
    }

    #[test]
    fn parses_a_dual_interface() {
        let l = parse(&sample()).unwrap();
        assert_eq!(l.name, "Lib");
        let i = &l.infos[0];
        assert_eq!(
            (i.name.as_str(), i.kind, i.cb_vft, i.impls.as_slice()),
            ("IThing", 4, 36, &[1u32][..])
        );
        assert_eq!(&i.guid[..], &(16..32u8).collect::<Vec<_>>()[..]);
        let f = &i.funcs;
        assert_eq!(
            (
                f[0].name.as_str(),
                f[0].memid,
                f[0].ovft,
                f[0].callconv,
                f[0].funckind
            ),
            ("getX", 0x6002_0000, 28, 4, 1)
        );
        assert_eq!(f[0].params[0].td, Td::Ptr(Box::new(Td::Base(VT_I4))));
        assert_eq!(
            (f[0].params[0].flags, f[0].params[0].name.as_str()),
            (0xa, "v")
        );
        assert_eq!(f[0].ret, Td::Base(25));
        assert_eq!((f[1].name.as_str(), f[1].ovft), ("setName", 32));
        assert_eq!(f[1].params[0].td, Td::Base(VT_BSTR));
    }

    /// Cut anywhere, a library parses or is refused without panicking.
    #[test]
    fn truncated_libraries_do_not_panic() {
        let d = sample();
        for n in 0..d.len() {
            let _ = parse(&d[..n]);
        }
        let mut seed = 7u64;
        for _ in 0..2000 {
            let mut e = d.clone();
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let at = (seed % e.len() as u64) as usize;
            e[at] = (seed >> 32) as u8;
            let _ = parse(&e);
        }
    }

    /// SAKE_TLB=path: a real library parses, and its first interface lists its functions.
    #[test]
    fn real_library() {
        let Ok(p) = std::env::var("SAKE_TLB") else {
            return;
        };
        let l = parse(&std::fs::read(p).unwrap()).unwrap();
        for i in l.infos.iter().take(3) {
            eprintln!(
                "{} kind {} funcs {} impls {:?}",
                i.name,
                i.kind,
                i.funcs.len(),
                i.impls
            );
            for f in i.funcs.iter().take(4) {
                eprintln!("  {:#x} {} vft {} {:?}", f.memid, f.name, f.ovft, f.params);
            }
        }
        assert!(l
            .infos
            .iter()
            .all(|i| i.funcs.iter().all(|f| !f.name.is_empty())));
    }

    #[test]
    fn variant_coercions() {
        assert_eq!(Var::Str("42".encode_utf16().collect()).int(), 42);
        assert_eq!(Var::R8(2.6).int(), 3);
        assert_eq!(Var::Bool(true).int(), -1);
        assert!(!Var::I4(0).truthy());
        assert_eq!(String::from_utf16_lossy(&Var::I4(-7).text()), "-7");
    }
}
