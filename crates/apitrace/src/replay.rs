//! A D3D8 / D3D9 / D3D11 trace (apitrace, 32- or 64-bit) as the call stream
//! tests/replay/replay.c plays back (on real Windows or on sake): every call that changes
//! something, with its arguments ready to pass (objects as handles, pointed-to structs and arrays
//! packed in the C layout of the trace's pointer width, from the headers' own layouts: layout.rs),
//! and the bytes apitrace saw written into each locked or mapped region.
//!
//! File: "IPRS", u32 version (2), u32 pointer width (32 / 64), then records (little endian):
//!   1 NAME     u32 id, u16 length, the name ("IDirect3DDevice9::Clear"), i32 vtable index (-1:
//!              an entry point, found by name in the D3D DLLs), u8 flags (1: returns an HRESULT;
//!              2: an entry point returning its object: the call's last argument is an OUTOBJ
//!              for it, not passed)
//!   2 CALL     u32 call number, u32 name id, u32 recorded return value, u8 flags (1: has one),
//!              u8 argument count, then each argument: u8 kind and
//!                WORD u64 | HANDLE u32 | BLOB u32 length, bytes, u16 fixups, each u32 offset,
//!                u8 kind (0: the object of handle v, 1: the blob's own address + v, 2: the
//!                window), u32 v; the pointer-width word at the offset is written |
//!                OUTOBJ u32 handle (a pointer-width slot zeroed; what the call leaves there is
//!                the object, or a D3D8 shader handle) | LOCK u8 type (0: void* of a buffer,
//!                1: D3DLOCKED_RECT, 2: D3DLOCKED_BOX, 3: D3D11_MAPPED_SUBRESOURCE), u32 handle of
//!                the locked object, u32 lock key | SCRATCH (4 KiB zeroed) | WINDOW (the
//!                replay's) | OUTARR u32 n, n u32 handles (n pointer-width slots)
//!              (a method's first argument is its object, a HANDLE)
//!   3 MEMCPY   u32 handle, u32 lock key, u32 offset, u32 recorded row pitch (0: linear), u32
//!              recorded slice pitch, u32 length, bytes
//!   4 WINSIZE  u32 width, u32 height: the back buffer size of the next call's device or swap
//!              chain (the window becomes that big)
//! A lock key tells the locks of one object apart: a texture's level, a cube face + 6 * level,
//! a D3D11 subresource. Calls without effects (Get*, Check*, ...) are left out unless they hand
//! out objects; a replay compares what it draws.

use crate::layout::{Layouts, Method, Record as Layout, Ty};
use crate::trace::{Call, Parser, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const MAGIC: &[u8; 4] = b"IPRS";
pub const VERSION: u32 = 2;

pub const NAME: u8 = 1;
pub const CALL: u8 = 2;
pub const MEMCPY: u8 = 3;
pub const WINSIZE: u8 = 4;

pub const WORD: u8 = 0;
pub const HANDLE: u8 = 1;
pub const BLOB: u8 = 2;
pub const OUTOBJ: u8 = 3;
pub const LOCK: u8 = 4;
pub const SCRATCH: u8 = 7;
pub const WINDOW: u8 = 8;
pub const OUTARR: u8 = 9;

pub const LOCK_BUF: u8 = 0;
pub const LOCK_RECT: u8 = 1;
pub const LOCK_BOX: u8 = 2;
pub const LOCK_MAP: u8 = 3;

pub const FIX_OBJ: u8 = 0;
pub const FIX_PTR: u8 = 1;
pub const FIX_WINDOW: u8 = 2;

pub const NAME_HR: u8 = 1;
pub const NAME_RETOBJ: u8 = 2;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Blob {
    pub bytes: Vec<u8>,
    /// (offset, kind, value)
    pub fix: Vec<(u32, u8, u32)>,
}

impl Blob {
    pub fn raw(bytes: Vec<u8>) -> Blob {
        Blob { bytes, fix: vec![] }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Word(u64),
    Handle(u32),
    Blob(Blob),
    OutObj(u32),
    Lock { kind: u8, handle: u32, key: u32 },
    Scratch,
    Window,
    OutArr(Vec<u32>),
}

#[derive(Default, Debug)]
pub struct Stats {
    pub calls: u64,
    pub written: u64,
    pub memcpys: u64,
    pub ptrbits: u32,
    /// calls left out on purpose (no effect on what is drawn), by name
    pub skipped: BTreeMap<String, u64>,
    /// calls this converter cannot replay (unknown functions, memcpy into no lock), by name
    pub dropped: BTreeMap<String, u64>,
}

/// The pointer width of a trace: 64 when any object or region it saw lies above 4 GiB (64-bit
/// Windows puts its heaps there), 32 otherwise
pub fn ptrbits(events: &[u8]) -> Result<u32, String> {
    let mut p = Parser::new(events)?;
    fn high(v: &Value) -> bool {
        match v {
            Value::Opaque(a) => *a > u32::MAX as u64,
            Value::Array(a) => a.iter().any(high),
            Value::Struct(_, m) => m.iter().any(high),
            _ => false,
        }
    }
    while let Some(c) = p.next_call()? {
        if c.args_in.iter().chain(&c.args_out).flatten().chain(&c.ret).any(high) {
            return Ok(64);
        }
    }
    Ok(32)
}

/// Converts a whole decompressed trace (`bits`: its pointer width, None: found by `ptrbits`)
pub fn convert(events: &[u8], bits: Option<u32>) -> Result<(Vec<u8>, Stats), String> {
    let seen = ptrbits(events)?;
    let bits = bits.unwrap_or(seen);
    if bits != 32 && bits != 64 {
        return Err(format!("a pointer width of {bits}"));
    }
    if bits == 32 && seen == 64 {
        return Err("a 64-bit trace (addresses above 4 GiB) converted as 32-bit".into());
    }
    let mut p = Parser::new(events)?;
    let mut c = Converter::new(bits);
    c.out.extend_from_slice(MAGIC);
    c.out.extend_from_slice(&VERSION.to_le_bytes());
    c.out.extend_from_slice(&bits.to_le_bytes());
    while let Some(call) = p.next_call()? {
        c.call(&call)?;
    }
    Ok((c.out, c.stats))
}

struct Converter {
    out: Vec<u8>,
    stats: Stats,
    /// 0: 32-bit, 1: 64-bit
    arch: usize,
    l: &'static Layouts,
    names: HashMap<String, u32>,
    next: u32,
    /// object address -> handle
    objects: HashMap<u64, u32>,
    /// a D3D8 shader handle (a DWORD) -> handle
    shaders: HashMap<u64, u32>,
    /// D3D11 buffers (mapped linearly)
    buffers: HashSet<u32>,
    /// locked region address -> (handle, key, row pitch, slice pitch)
    locks: BTreeMap<u64, (u32, u32, u32, u32)>,
    winsize: Option<(u32, u32)>,
}

/// methods that only read (their answers are what the game saw, not what it drew)
fn no_effect(method: &str) -> bool {
    // (D3D11's per-stage Gets: OMGetRenderTargets, PSGetShaderResources, ...)
    let b = method.as_bytes();
    let method = if b.len() > 5 && b[0].is_ascii_uppercase() && b[1].is_ascii_uppercase() && method[2..].starts_with("Get") { &method[2..] } else { method };
    if ["GetRenderTargetData", "GetFrontBufferData", "GetDC"].contains(&method) {
        return false;
    }
    ["Get", "Check", "Validate", "Enum", "TestCooperativeLevel", "SetPrivateData", "FreePrivateData", "SetFullscreenState", "QueryResourceResidency"].iter().any(|p| method.starts_with(p)) || method == "ShowCursor"
}

fn opaque(v: &Value) -> Option<u64> {
    match v {
        Value::Opaque(a) => Some(*a),
        v => match v.deref() {
            Some(Value::Opaque(a)) => Some(*a),
            _ => None,
        },
    }
}

/// an object (non-NULL) among what an out argument holds
fn hands_out(v: &Value) -> bool {
    match v {
        Value::Opaque(a) => *a != 0,
        Value::Array(a) => a.iter().any(hands_out),
        _ => false,
    }
}

impl Converter {
    fn new(bits: u32) -> Converter {
        Converter {
            out: vec![],
            stats: Stats { ptrbits: bits, ..Stats::default() },
            arch: (bits == 64) as usize,
            l: Layouts::get(),
            names: HashMap::new(),
            next: 0,
            objects: HashMap::new(),
            shaders: HashMap::new(),
            buffers: HashSet::new(),
            locks: BTreeMap::new(),
            winsize: None,
        }
    }

    fn call(&mut self, call: &Call) -> Result<(), String> {
        self.stats.calls += 1;
        let name = call.sig.name.as_str();
        if name == "memcpy" {
            return self.memcpy(call);
        }
        let l = self.l;
        let (iface, method) = match name.split_once("::") {
            Some((i, m)) => (Some(i), m),
            None => (None, name),
        };
        let (params, ret_ty, index) = match iface {
            None => match l.functions.get(name) {
                Some((r, p)) => (&p[..], r, -1),
                None => {
                    *if name.starts_with("D3DPERF_") { &mut self.stats.skipped } else { &mut self.stats.dropped }.entry(name.into()).or_default() += 1;
                    return Ok(());
                }
            },
            Some(i) => match l.method(i, method) {
                Some(Method { params, ret, index, .. }) => (&params[..], ret, *index as i32),
                None => {
                    *self.stats.dropped.entry(name.into()).or_default() += 1;
                    return Ok(());
                }
            },
        };
        let first = iface.is_some() as usize;
        if call.sig.args.len() > params.len() + first {
            return Err(format!("{name}: {} arguments recorded, the header has {}", call.sig.args.len(), params.len() + first));
        }
        // (a call without effects is kept when it hands out objects: their Release is replayed)
        if iface.is_some() && no_effect(method) && !call.args_out.iter().flatten().any(hands_out) {
            *self.stats.skipped.entry(name.into()).or_default() += 1;
            return Ok(());
        }
        let mut args = Vec::with_capacity(params.len() + 2);
        if iface.is_some() {
            args.push(match call.args_in.first().and_then(|v| v.as_ref()) {
                Some(Value::Opaque(a)) => Arg::Handle(self.handle(*a)),
                _ => Arg::Word(0),
            });
        }
        for (k, t) in params.iter().enumerate() {
            let i = k + first;
            let arg_name = call.sig.args.get(i).map(|s| s.as_str()).unwrap_or("");
            let vin = call.args_in.get(i).and_then(|v| v.as_ref());
            let vout = call.args_out.get(i).and_then(|v| v.as_ref());
            let a = self.arg(call, iface.unwrap_or(""), method, arg_name, t, vin, vout).map_err(|e| format!("call {} {name} {arg_name}: {e}", call.no))?;
            args.push(a);
        }
        // a vertex declaration ends with D3DDECL_END (apitrace may count it out)
        if method == "CreateVertexDeclaration" {
            if let Some(Arg::Blob(b)) = args.get_mut(1) {
                let b = &mut b.bytes;
                if b.len() < 8 || b[b.len() - 8..b.len() - 6] != [0xff, 0] {
                    b.extend_from_slice(&[0xff, 0, 0, 0, 17, 0, 0, 0]);
                }
            }
        }
        let ret = call.ret.as_ref();
        let mut flags = 0;
        let mut ret_word = None;
        match ret_ty {
            Ty::Ptr(o) if matches!(**o, Ty::Obj(_)) && iface.is_none() => {
                // Direct3DCreate9 gives its object as the return value
                flags |= NAME_RETOBJ;
                args.push(Arg::OutObj(match ret {
                    Some(Value::Opaque(a)) if *a != 0 => self.handle(*a),
                    _ => 0,
                }));
            }
            Ty::Hr => {
                flags |= NAME_HR;
                ret_word = ret.and_then(|r| r.int()).map(|r| r as u32);
            }
            _ => {}
        }
        if method == "CreateBuffer" {
            if let Some(Arg::OutObj(h)) = args.last() {
                self.buffers.insert(*h);
            }
        }
        let id = self.name(name, index, flags);
        if let Some((w, h)) = self.winsize.take() {
            self.out.push(WINSIZE);
            self.out.extend_from_slice(&w.to_le_bytes());
            self.out.extend_from_slice(&h.to_le_bytes());
        }
        self.out.push(CALL);
        self.out.extend_from_slice(&(call.no as u32).to_le_bytes());
        self.out.extend_from_slice(&id.to_le_bytes());
        self.out.extend_from_slice(&ret_word.unwrap_or(0).to_le_bytes());
        self.out.push(ret_word.is_some() as u8);
        if args.len() > 255 {
            return Err(format!("{name}: {} arguments", args.len()));
        }
        self.out.push(args.len() as u8);
        for a in &args {
            write_arg(&mut self.out, a);
        }
        self.stats.written += 1;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn arg(&mut self, call: &Call, iface: &str, method: &str, arg_name: &str, t: &Ty, vin: Option<&Value>, vout: Option<&Value>) -> Result<Arg, String> {
        // (no fullscreen mode; a non-Ex D3D9 device wants pSharedHandle NULL)
        if matches!(arg_name, "pFullscreenDisplayMode" | "pFullscreenDesc" | "pSharedHandle") {
            return Ok(Arg::Word(0));
        }
        let Ty::Ptr(inner) = t else {
            // a value: an integer, a float, a window, a D3D8 shader handle
            let Some(v) = vin else { return Ok(Arg::Word(0)) };
            let shader = |c: &Converter| iface.starts_with("IDirect3DDevice8") && arg_name == "Handle" && v.int().is_some_and(|h| c.shaders.contains_key(&(h as u64)));
            return Ok(match (t, v) {
                (Ty::Hwnd, Value::Opaque(a)) if *a != 0 => Arg::Window,
                (Ty::Int(4), v) if shader(self) => Arg::Handle(self.shaders[&(v.int().unwrap() as u64)]),
                (_, Value::Float(f)) => Arg::Word(f.to_bits() as u64),
                (_, Value::Double(f)) => Arg::Word(f.to_bits()),
                (_, Value::Null) | (_, Value::Opaque(_)) => Arg::Word(0),
                // (an integer of the parameter's width: a negative DWORD is 0xffffffff.., not
                // sign-extended to 64 bits)
                (Ty::Int(w), v) => Arg::Word(v.int().ok_or(format!("a value {v}"))? as u64 & (u64::MAX >> (64 - 8 * *w as u32))),
                (_, v) => Arg::Word(v.int().ok_or(format!("a value {v}"))? as u64),
            });
        };
        if let Some(v) = vin {
            return Ok(match v {
                Value::Null => Arg::Word(0),
                Value::Opaque(a) => match self.objects.get(a) {
                    Some(&h) if matches!(**inner, Ty::Obj(_) | Ty::Void) => Arg::Handle(h),
                    _ => Arg::Word(0),
                },
                v => {
                    let mut p = Packer::with(self.l, self.arch, &self.objects);
                    p.pointee(inner, v)?;
                    let mut blob = p.finish();
                    if let Ty::Rec(r) = &**inner {
                        self.patch(r, &mut blob);
                    }
                    Arg::Blob(blob)
                }
            });
        }
        // an out argument (a place to write is passed: the call may write there, even NULL
        // after a failure)
        let pp = arg_name.starts_with("pp");
        let v = match vout {
            None | Some(Value::Null) => return Ok(if pp { Arg::OutObj(0) } else { Arg::Word(0) }),
            Some(v) => v,
        };
        let this = match call.args_in.first().and_then(|v| v.as_ref()) {
            Some(Value::Opaque(a)) => self.handle(*a),
            _ => 0,
        };
        let int_arg = |n: &str| call.arg(n).and_then(|v| v.int());
        match (&**inner, v.deref()) {
            // a locked buffer: the address of its bytes
            (Ty::Ptr(_), Some(Value::Opaque(a))) if method == "Lock" => {
                self.locks.insert(*a, (this, 0, 0, 0));
                return Ok(Arg::Lock { kind: LOCK_BUF, handle: this, key: 0 });
            }
            (Ty::Rec(r), Some(s @ Value::Struct(..))) if r.ends_with("LOCKED_RECT") || r.ends_with("LOCKED_BOX") => {
                let level = int_arg("Level").unwrap_or(0) as u32;
                let key = int_arg("FaceType").map_or(level, |f| f as u32 + level * 6);
                let rect = r.ends_with("LOCKED_RECT");
                let pitch = s.member(if rect { "Pitch" } else { "RowPitch" }).and_then(|v| v.int()).unwrap_or(0) as u32;
                let slice = if rect { 0 } else { s.member("SlicePitch").and_then(|v| v.int()).unwrap_or(0) as u32 };
                if let Some(Value::Opaque(a)) = s.member("pBits") {
                    self.locks.insert(*a, (this, key, pitch, slice));
                }
                return Ok(Arg::Lock { kind: if rect { LOCK_RECT } else { LOCK_BOX }, handle: this, key });
            }
            (Ty::Rec(r), Some(s @ Value::Struct(..))) if r == "D3D11_MAPPED_SUBRESOURCE" => {
                let target = match call.arg("pResource") {
                    Some(Value::Opaque(a)) => self.handle(*a),
                    _ => this,
                };
                let key = int_arg("Subresource").unwrap_or(0) as u32;
                let linear = self.buffers.contains(&target);
                let pitch = if linear { 0 } else { s.member("RowPitch").and_then(|v| v.int()).unwrap_or(0) as u32 };
                let slice = if linear { 0 } else { s.member("DepthPitch").and_then(|v| v.int()).unwrap_or(0) as u32 };
                if let Some(Value::Opaque(a)) = s.member("pData") {
                    self.locks.insert(*a, (target, key, pitch, slice));
                }
                return Ok(Arg::Lock { kind: LOCK_MAP, handle: target, key });
            }
            // a D3D8 shader's handle
            (Ty::Int(4), Some(h)) if iface.starts_with("IDirect3DDevice8") && method.starts_with("Create") && method.ends_with("Shader") => {
                let n = self.new_handle();
                self.shaders.insert(h.int().unwrap_or(0) as u64, n);
                return Ok(Arg::OutObj(n));
            }
            _ => {}
        }
        if matches!(**inner, Ty::Ptr(_)) {
            // objects handed out: one, or an array of them
            let mut h = |a: Option<u64>| match a {
                Some(0) | None => 0,
                Some(a) => self.handle(a),
            };
            return Ok(match v {
                Value::Array(a) if a.len() > 1 => Arg::OutArr(a.iter().map(|x| h(opaque(x))).collect()),
                v => Arg::OutObj(h(opaque(v))),
            });
        }
        // an InOut argument (apitrace writes it at the LEAVE only): what the call left there,
        // e.g. D3DPRESENT_PARAMETERS with the back buffer size filled in
        if let Ty::Rec(r) = &**inner {
            let mut p = Packer::with(self.l, self.arch, &self.objects);
            if p.pointee(inner, v).is_ok() {
                let mut blob = p.finish();
                self.patch(r, &mut blob);
                return Ok(Arg::Blob(blob));
            }
        }
        Ok(Arg::Scratch)
    }

    /// what a replay changes in what it passes: windowed, in its own window, no vsync
    fn patch(&mut self, rec: &str, b: &mut Blob) {
        let Some(r) = self.l.records.get(rec) else { return };
        let arch = self.arch;
        let off = |name: &str| r.members.iter().find(|m| m.name == name).map(|m| m.off[arch] as usize);
        let set = |b: &mut Blob, name: &str, v: u32| {
            if let Some(o) = off(name) {
                b.bytes[o..o + 4].copy_from_slice(&v.to_le_bytes());
            }
        };
        let get = |b: &Blob, o: usize| u32::from_le_bytes(b.bytes[o..o + 4].try_into().unwrap());
        let window = |b: &mut Blob, name: &str| {
            if let Some(o) = off(name) {
                b.fix.retain(|f| f.0 != o as u32);
                b.fix.push((o as u32, FIX_WINDOW, 0));
            }
        };
        match rec {
            "_D3DPRESENT_PARAMETERS_" | "8._D3DPRESENT_PARAMETERS_" => {
                set(b, "Windowed", 1);
                set(b, "FullScreen_RefreshRateInHz", 0);
                // (D3D9: no vsync; D3D8 wants DEFAULT when windowed)
                set(b, "PresentationInterval", 0x8000_0000);
                set(b, "FullScreen_PresentationInterval", 0);
                window(b, "hDeviceWindow");
                self.winsize = Some((get(b, 0), get(b, 4)));
            }
            "DXGI_SWAP_CHAIN_DESC" => {
                set(b, "Windowed", 1);
                window(b, "OutputWindow");
                self.winsize = Some((get(b, 0), get(b, 4)));
            }
            "DXGI_SWAP_CHAIN_DESC1" => self.winsize = Some((get(b, 0), get(b, 4))),
            _ => {}
        }
    }

    fn new_handle(&mut self) -> u32 {
        self.next += 1;
        self.next
    }

    fn handle(&mut self, addr: u64) -> u32 {
        if let Some(&h) = self.objects.get(&addr) {
            return h;
        }
        let h = self.new_handle();
        self.objects.insert(addr, h);
        h
    }

    fn name(&mut self, name: &str, index: i32, flags: u8) -> u32 {
        if let Some(&id) = self.names.get(name) {
            return id;
        }
        let id = self.names.len() as u32 + 1;
        self.names.insert(name.into(), id);
        self.out.push(NAME);
        self.out.extend_from_slice(&id.to_le_bytes());
        self.out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        self.out.extend_from_slice(name.as_bytes());
        self.out.extend_from_slice(&index.to_le_bytes());
        self.out.push(flags);
        id
    }

    fn memcpy(&mut self, call: &Call) -> Result<(), String> {
        let (Some(Value::Opaque(dest)), Some(Value::Blob(data))) = (call.args_in.first().and_then(|v| v.as_ref()), call.args_in.get(1).and_then(|v| v.as_ref())) else {
            *self.stats.dropped.entry("memcpy (no destination or bytes)".into()).or_default() += 1;
            return Ok(());
        };
        let Some((&base, &(h, key, pitch, slice))) = self.locks.range(..=*dest).next_back() else {
            *self.stats.dropped.entry("memcpy (into no lock)".into()).or_default() += 1;
            return Ok(());
        };
        self.out.push(MEMCPY);
        for w in [h, key, (*dest - base) as u32, pitch, slice, data.len() as u32] {
            self.out.extend_from_slice(&w.to_le_bytes());
        }
        self.out.extend_from_slice(data);
        self.stats.memcpys += 1;
        Ok(())
    }
}

/// A pointed-to value laid out as the C code of the trace's pointer width had it: the value
/// first, what its own pointers point to after it (each fixed up to its place when replayed)
pub struct Packer<'a> {
    l: &'a Layouts,
    arch: usize,
    objects: &'a HashMap<u64, u32>,
    b: Blob,
}

impl<'a> Packer<'a> {
    pub fn with(l: &'a Layouts, arch: usize, objects: &'a HashMap<u64, u32>) -> Packer<'a> {
        Packer { l, arch, objects, b: Blob::default() }
    }

    pub fn finish(self) -> Blob {
        self.b
    }

    /// the value (or the array of them) a pointer to t points to, laid out at the end; where
    pub fn pointee(&mut self, t: &Ty, v: &Value) -> Result<u32, String> {
        while !self.b.bytes.len().is_multiple_of(8) {
            self.b.bytes.push(0);
        }
        let start = self.b.bytes.len() as u32;
        match v {
            Value::Blob(x) => self.b.bytes.extend_from_slice(x),
            Value::Str(x) => self.b.bytes.extend(x.iter().copied().chain([0])),
            Value::WStr(x) => self.b.bytes.extend(x.iter().flat_map(|&c| (c as u16).to_le_bytes()).chain([0, 0])),
            Value::Repr(_, m) => return self.pointee(t, m),
            Value::Array(a) => {
                let size = self.elem_size(t)?;
                self.b.bytes.resize(start as usize + size as usize * a.len(), 0);
                for (i, x) in a.iter().enumerate() {
                    self.put(start + i as u32 * size, t, x)?;
                }
            }
            v => {
                let size = self.elem_size(t)?;
                self.b.bytes.resize(start as usize + size as usize, 0);
                self.put(start, t, v)?;
            }
        }
        Ok(start)
    }

    fn elem_size(&self, t: &Ty) -> Result<u32, String> {
        match t {
            // (void*: bytes)
            Ty::Void => Ok(1),
            t => self.l.size(t, self.arch),
        }
    }

    fn word(&mut self, off: u32, w: usize, x: u64) {
        let o = off as usize;
        self.b.bytes[o..o + w].copy_from_slice(&x.to_le_bytes()[..w]);
    }

    /// a value of type t at off (inside what is already laid out)
    pub fn put(&mut self, off: u32, t: &Ty, v: &Value) -> Result<(), String> {
        match (t, v) {
            (_, Value::Null) => {}
            (Ty::Int(w), v) => {
                let x = match v {
                    Value::Float(f) => *f as i64,
                    v => v.int().ok_or(format!("an integer {v}"))?,
                };
                self.word(off, *w as usize, x as u64);
            }
            (Ty::F4, Value::Float(f)) => self.word(off, 4, f.to_bits() as u64),
            (Ty::F4, Value::Double(f)) => self.word(off, 4, (*f as f32).to_bits() as u64),
            (Ty::F4, v) => self.word(off, 4, (v.int().ok_or(format!("a float {v}"))? as f32).to_bits() as u64),
            (Ty::F8, Value::Double(f)) => self.word(off, 8, f.to_bits()),
            (Ty::F8, Value::Float(f)) => self.word(off, 8, (*f as f64).to_bits()),
            (Ty::Hwnd, Value::Opaque(a)) => {
                if *a != 0 {
                    self.b.fix.push((off, FIX_WINDOW, 0));
                }
            }
            // a pointer to something apitrace did not follow: an object's, or nothing
            (Ty::Ptr(_) | Ty::Fn, Value::Opaque(a)) => {
                if let Some(&h) = self.objects.get(a) {
                    self.b.fix.push((off, FIX_OBJ, h));
                }
            }
            (Ty::Ptr(p), v) => {
                let at = self.pointee(p, v)?;
                self.b.fix.push((off, FIX_PTR, at));
            }
            (Ty::Rec(r), Value::Struct(sig, m)) => {
                let rec = self.l.records.get(r).ok_or(format!("no layout of {r}"))?;
                for (name, x) in sig.members.iter().zip(m) {
                    let (o, mt) = member(self.l, rec, if name.is_empty() { "-" } else { name }, self.arch).ok_or(format!("{r}: no member {name:?}"))?;
                    self.put(off + o, mt, x)?;
                }
            }
            (Ty::Arr(n, e), Value::Array(a)) => {
                let size = self.l.size(e, self.arch)?;
                if a.len() > *n as usize {
                    return Err(format!("{} elements in an array of {n}", a.len()));
                }
                for (i, x) in a.iter().enumerate() {
                    self.put(off + i as u32 * size, e, x)?;
                }
            }
            (Ty::Arr(..), Value::Blob(x) | Value::Str(x)) => {
                let k = x.len().min(self.l.size(t, self.arch)? as usize);
                self.b.bytes[off as usize..off as usize + k].copy_from_slice(&x[..k]);
            }
            (Ty::Void, _) => {}
            (t, v) => return Err(format!("a {v} as {t:?} ({}-bit)", [32, 64][self.arch])),
        }
        Ok(())
    }
}

/// a struct's member by name (offset, type), looked for in its anonymous members too
fn member<'a>(l: &'a Layouts, rec: &'a Layout, name: &str, arch: usize) -> Option<(u32, &'a Ty)> {
    if let Some(m) = rec.members.iter().find(|m| m.name == name) {
        return Some((m.off[arch], &m.ty));
    }
    for m in rec.members.iter().filter(|m| m.name == "-") {
        if let Ty::Rec(r) = &m.ty {
            if let Some((o, t)) = l.records.get(r).and_then(|r| member(l, r, name, arch)) {
                return Some((m.off[arch] + o, t));
            }
        }
    }
    None
}

fn write_arg(out: &mut Vec<u8>, a: &Arg) {
    let w32 = |out: &mut Vec<u8>, w: u32| out.extend_from_slice(&w.to_le_bytes());
    match a {
        Arg::Word(w) => {
            out.push(WORD);
            out.extend_from_slice(&w.to_le_bytes());
        }
        Arg::Handle(h) => {
            out.push(HANDLE);
            w32(out, *h);
        }
        Arg::Blob(b) => {
            out.push(BLOB);
            w32(out, b.bytes.len() as u32);
            out.extend_from_slice(&b.bytes);
            out.extend_from_slice(&(b.fix.len() as u16).to_le_bytes());
            for &(o, k, v) in &b.fix {
                w32(out, o);
                out.push(k);
                w32(out, v);
            }
        }
        Arg::OutObj(h) => {
            out.push(OUTOBJ);
            w32(out, *h);
        }
        Arg::Lock { kind, handle, key } => {
            out.push(LOCK);
            out.push(*kind);
            w32(out, *handle);
            w32(out, *key);
        }
        Arg::Scratch => out.push(SCRATCH),
        Arg::Window => out.push(WINDOW),
        Arg::OutArr(hs) => {
            out.push(OUTARR);
            w32(out, hs.len() as u32);
            for &h in hs {
                w32(out, h);
            }
        }
    }
}

/// A replay stream read back (for the tests)
#[derive(Debug, PartialEq)]
pub enum Record {
    Name { id: u32, name: String, index: i32, flags: u8 },
    Call { no: u32, name: String, ret: Option<u32>, args: Vec<Arg> },
    Memcpy { handle: u32, key: u32, offset: u32, pitch: u32, slice: u32, data: Vec<u8> },
    WinSize(u32, u32),
}

/// (pointer width, records)
pub fn read(b: &[u8]) -> Result<(u32, Vec<Record>), String> {
    if b.get(..4) != Some(MAGIC) || b.get(4..8) != Some(&VERSION.to_le_bytes()) {
        return Err("not a replay stream of this version".into());
    }
    let mut i = 8;
    let bytes = |i: &mut usize, n: usize| -> Result<Vec<u8>, String> {
        let v = b.get(*i..i.checked_add(n).ok_or("replay: past the end")?).ok_or("replay: past the end")?.to_vec();
        *i += n;
        Ok(v)
    };
    let u8_ = |i: &mut usize| -> Result<u8, String> { Ok(bytes(i, 1)?[0]) };
    let u32_ = |i: &mut usize| -> Result<u32, String> { Ok(u32::from_le_bytes(bytes(i, 4)?.try_into().unwrap())) };
    let bits = u32_(&mut i)?;
    let mut names = HashMap::new();
    let mut out = vec![];
    while i < b.len() {
        match u8_(&mut i)? {
            NAME => {
                let id = u32_(&mut i)?;
                let n = u16::from_le_bytes(bytes(&mut i, 2)?.try_into().unwrap()) as usize;
                let name = String::from_utf8(bytes(&mut i, n)?).map_err(|e| e.to_string())?;
                let index = u32_(&mut i)? as i32;
                let flags = u8_(&mut i)?;
                names.insert(id, name.clone());
                out.push(Record::Name { id, name, index, flags });
            }
            CALL => {
                let no = u32_(&mut i)?;
                let name = names.get(&u32_(&mut i)?).ok_or("replay: unknown name")?.clone();
                let r = u32_(&mut i)?;
                let ret = (u8_(&mut i)? & 1 != 0).then_some(r);
                let n = u8_(&mut i)?;
                let mut args = vec![];
                for _ in 0..n {
                    args.push(match u8_(&mut i)? {
                        WORD => Arg::Word(u64::from_le_bytes(bytes(&mut i, 8)?.try_into().unwrap())),
                        HANDLE => Arg::Handle(u32_(&mut i)?),
                        BLOB => {
                            let n = u32_(&mut i)? as usize;
                            let data = bytes(&mut i, n)?;
                            let k = u16::from_le_bytes(bytes(&mut i, 2)?.try_into().unwrap());
                            let mut fix = vec![];
                            for _ in 0..k {
                                fix.push((u32_(&mut i)?, u8_(&mut i)?, u32_(&mut i)?));
                            }
                            Arg::Blob(Blob { bytes: data, fix })
                        }
                        OUTOBJ => Arg::OutObj(u32_(&mut i)?),
                        LOCK => Arg::Lock { kind: u8_(&mut i)?, handle: u32_(&mut i)?, key: u32_(&mut i)? },
                        SCRATCH => Arg::Scratch,
                        WINDOW => Arg::Window,
                        OUTARR => {
                            let n = u32_(&mut i)?;
                            Arg::OutArr((0..n).map(|_| u32_(&mut i)).collect::<Result<_, _>>()?)
                        }
                        k => return Err(format!("replay: argument kind {k}")),
                    });
                }
                out.push(Record::Call { no, name, ret, args });
            }
            MEMCPY => {
                let w: Vec<u32> = (0..6).map(|_| u32_(&mut i)).collect::<Result<_, _>>()?;
                let data = bytes(&mut i, w[5] as usize)?;
                out.push(Record::Memcpy { handle: w[0], key: w[1], offset: w[2], pitch: w[3], slice: w[4], data });
            }
            WINSIZE => out.push(Record::WinSize(u32_(&mut i)?, u32_(&mut i)?)),
            t => return Err(format!("replay: record {t}")),
        }
    }
    Ok((bits, out))
}
