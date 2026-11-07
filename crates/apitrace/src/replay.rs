//! A D3D9 trace (apitrace, 32-bit) as the call stream tests/replay/d3d9replay.c plays back on
//! a D3D9 (real Windows or sake): every call that changes something, with its arguments ready
//! to push (objects as handles, pointed-to structs and arrays packed as the 32-bit C layout),
//! and the bytes apitrace saw written into each locked region.
//!
//! File: "IPRS", u32 version, then records (little endian):
//!   1 NAME    u32 id, u16 length, the method's name ("IDirect3DDevice9::Clear")
//!   2 CALL    u32 call number, u32 name id, u32 recorded return value, u8 flags (1: has one),
//!             u8 argument count, then each argument: u8 kind and
//!               WORD u32 | HANDLE u32 | BLOB u32 length, bytes | OUTOBJ u32 handle |
//!               LOCKBUF/LOCKRECT/LOCKBOX u32 lock key | SCRATCH | WINDOW
//!             (the first argument of a method is the object, a HANDLE)
//!   3 MEMCPY  u32 handle, u32 lock key, u32 offset, u32 recorded row pitch (0: a buffer),
//!             u32 recorded slice pitch, u32 length, bytes
//! A lock key tells the locks of one object apart: a texture's level, a cube face + 6 * level.
//! Calls without effects (Get*, Check*, ...) are left out; a replay compares what it draws.

use crate::trace::{Call, Parser, Value};
use std::collections::{BTreeMap, HashMap};

pub const MAGIC: &[u8; 4] = b"IPRS";
pub const VERSION: u32 = 1;

pub const NAME: u8 = 1;
pub const CALL: u8 = 2;
pub const MEMCPY: u8 = 3;

pub const WORD: u8 = 0;
pub const HANDLE: u8 = 1;
pub const BLOB: u8 = 2;
pub const OUTOBJ: u8 = 3;
pub const LOCKBUF: u8 = 4;
pub const LOCKRECT: u8 = 5;
pub const LOCKBOX: u8 = 6;
pub const SCRATCH: u8 = 7;
pub const WINDOW: u8 = 8;

#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Word(u32),
    Handle(u32),
    Blob(Vec<u8>),
    OutObj(u32),
    Lock(u8, u32),
    Scratch,
    Window,
}

#[derive(Default, Debug)]
pub struct Stats {
    pub calls: u64,
    pub written: u64,
    pub memcpys: u64,
    /// calls left out on purpose (no effect on what is drawn), by name
    pub skipped: BTreeMap<String, u64>,
    /// calls this converter cannot replay (unknown functions, memcpy into no lock), by name
    pub dropped: BTreeMap<String, u64>,
}

/// Converts a whole decompressed trace
pub fn convert(events: &[u8]) -> Result<(Vec<u8>, Stats), String> {
    let mut p = Parser::new(events)?;
    let mut c = Converter::default();
    c.out.extend_from_slice(MAGIC);
    c.out.extend_from_slice(&VERSION.to_le_bytes());
    if let Some((_, arch)) = p.properties.iter().find(|(k, _)| k == "process.arch" || k == "arch") {
        if arch.contains("64") {
            return Err(format!("a {arch} trace: only 32-bit D3D9 traces replay"));
        }
    }
    while let Some(call) = p.next_call()? {
        c.call(&call)?;
    }
    Ok((c.out, c.stats))
}

#[derive(Default)]
struct Converter {
    out: Vec<u8>,
    stats: Stats,
    names: HashMap<String, u32>,
    /// object address -> handle
    objects: HashMap<u64, u32>,
    /// locked region address -> (handle, key, row pitch, slice pitch)
    locks: BTreeMap<u64, (u32, u32, u32, u32)>,
}

/// methods that only read (their answers are what the game saw, not what it drew)
fn no_effect(method: &str) -> bool {
    const KEEP: &[&str] = &["GetRenderTargetData", "GetFrontBufferData", "GetBackBuffer", "GetSurfaceLevel", "GetCubeMapSurface", "GetVolumeLevel", "GetRenderTarget", "GetDepthStencilSurface", "GetSwapChain", "GetDC", "GetDirect3D", "GetDevice", "GetContainer", "GetTexture", "GetVertexShader", "GetPixelShader", "GetVertexDeclaration", "GetStreamSource", "GetIndices"];
    if KEEP.contains(&method) {
        return false;
    }
    ["Get", "Check", "Validate", "Enum", "TestCooperativeLevel", "SetPrivateData", "FreePrivateData"].iter().any(|p| method.starts_with(p)) || method == "ShowCursor"
}

fn is_window(arg: &str) -> bool {
    arg == "hWnd" || (arg.starts_with('h') && arg.contains("Window"))
}

impl Converter {
    fn call(&mut self, call: &Call) -> Result<(), String> {
        self.stats.calls += 1;
        let name = call.sig.name.as_str();
        if ["IDirect3D8", "Direct3DCreate8", "ID3D1", "IDXGI", "D3D1", "CreateDXGI"].iter().any(|p| name.starts_with(p)) {
            return Err(format!("unsupported: {name} (D3D9 only so far)"));
        }
        if call.args_in.first().and_then(|v| v.as_ref()).is_some_and(|v| matches!(v, Value::Opaque(a) if *a > u32::MAX as u64)) {
            return Err(format!("unsupported: a 64-bit trace ({name})"));
        }
        if name == "memcpy" {
            return self.memcpy(call);
        }
        let (iface, method) = match name.split_once("::") {
            Some((i, m)) => (Some(i), m),
            None => (None, name),
        };
        if iface.is_none() && !matches!(name, "Direct3DCreate9" | "Direct3DCreate9Ex") {
            *if name.starts_with("D3DPERF_") { &mut self.stats.skipped } else { &mut self.stats.dropped }.entry(name.into()).or_default() += 1;
            return Ok(());
        }
        if no_effect(method) {
            *self.stats.skipped.entry(name.into()).or_default() += 1;
            return Ok(());
        }
        let n = call.sig.args.len().max(call.args_in.len()).max(call.args_out.len());
        let mut args = Vec::with_capacity(n);
        for i in 0..n {
            let arg_name = call.sig.args.get(i).map(|s| s.as_str()).unwrap_or("");
            let vin = call.args_in.get(i).and_then(|v| v.as_ref());
            let vout = call.args_out.get(i).and_then(|v| v.as_ref());
            args.push(self.arg(call, method, i, arg_name, vin, vout)?);
        }
        // a vertex declaration ends with D3DDECL_END (apitrace may count it out)
        if method == "CreateVertexDeclaration" {
            if let Some(Arg::Blob(b)) = args.get_mut(1) {
                if b.len() < 8 || b[b.len() - 8..b.len() - 6] != [0xff, 0] {
                    b.extend_from_slice(&[0xff, 0, 0, 0, 17, 0, 0, 0]);
                }
            }
        }
        // a new object's handle comes from the address it had (one address, one handle; a
        // freed address used again by a later object is that object's handle then)
        let ret = call.ret.as_ref();
        let mut ret_word = None;
        if iface.is_none() {
            // Direct3DCreate9 gives its object as the return value
            if let Some(Value::Opaque(a)) = ret {
                let h = self.handle(*a);
                args.push(Arg::OutObj(h));
            } else {
                args.push(Arg::OutObj(0));
            }
        } else if let Some(r) = ret.and_then(|r| r.int()) {
            ret_word = Some(r as u32);
        }
        let id = self.name(name);
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

    fn arg(&mut self, call: &Call, method: &str, i: usize, arg_name: &str, vin: Option<&Value>, vout: Option<&Value>) -> Result<Arg, String> {
        if vin.is_none() {
            // (neither in nor out: an argument apitrace does not record, e.g. pSharedHandle, which
            // a non-Ex device wants NULL)
            // An out argument with nothing recorded, or recorded as NULL: an object's (pp...)
            // after a failed call (a place to write is passed: the call may write NULL there), or
            // a pointer the caller passed as NULL (pSharedHandle, which a non-Ex device wants NULL)
            let pp = arg_name.starts_with("pp");
            let v = match vout {
                None | Some(Value::Null) => return Ok(if pp { Arg::OutObj(0) } else { Arg::Word(0) }),
                Some(v) => v,
            };
            let inner = v.deref();
            let level = call.arg("Level").and_then(|v| v.int()).unwrap_or(0) as u32;
            let face = call.arg("FaceType").and_then(|v| v.int()).map(|f| f as u32);
            return Ok(match inner {
                Some(Value::Opaque(a)) if method == "Lock" => {
                    let h = self.this(call);
                    let key = 0;
                    self.locks.insert(*a, (h, key, 0, 0));
                    Arg::Lock(LOCKBUF, key)
                }
                Some(s @ Value::Struct(sig, _)) if sig.name == "D3DLOCKED_RECT" || sig.name == "D3DLOCKED_BOX" => {
                    let h = self.this(call);
                    let key = face.map_or(level, |f| f + level * 6);
                    let bits = s.member("pBits");
                    let rect = sig.name == "D3DLOCKED_RECT";
                    let pitch = s.member(if rect { "Pitch" } else { "RowPitch" }).and_then(|v| v.int()).unwrap_or(0) as u32;
                    let slice = if rect { 0 } else { s.member("SlicePitch").and_then(|v| v.int()).unwrap_or(0) as u32 };
                    if let Some(Value::Opaque(a)) = bits {
                        self.locks.insert(*a, (h, key, pitch, slice));
                    }
                    Arg::Lock(if rect { LOCKRECT } else { LOCKBOX }, key)
                }
                Some(Value::Opaque(a)) => Arg::OutObj(self.handle(*a)),
                Some(Value::Null) if method.starts_with("Create") || method.starts_with("Get") => Arg::OutObj(0),
                // an InOut argument (apitrace writes it at the LEAVE only): what the call left
                // there, e.g. D3DPRESENT_PARAMETERS with the back buffer size filled in
                Some(Value::Struct(..)) | Some(Value::Array(_)) => {
                    let mut b = Vec::new();
                    match pack(&mut b, inner.unwrap(), 4) {
                        Ok(()) => Arg::Blob(b),
                        Err(_) => Arg::Scratch,
                    }
                }
                _ => Arg::Scratch,
            });
        }
        let v = vin.unwrap();
        if i == 0 && call.sig.name.contains("::") {
            // the object
            return Ok(match v {
                Value::Opaque(a) => Arg::Handle(self.handle(*a)),
                _ => Arg::Word(0),
            });
        }
        Ok(match v {
            Value::Null => Arg::Word(0),
            Value::Float(f) => Arg::Word(f.to_bits()),
            Value::Opaque(a) => match self.objects.get(a) {
                Some(&h) => Arg::Handle(h),
                None if is_window(arg_name) => Arg::Window,
                None => Arg::Word(*a as u32),
            },
            Value::Str(s) => Arg::Blob(s.iter().copied().chain([0]).collect()),
            Value::WStr(s) => Arg::Blob(s.iter().flat_map(|&c| (c as u16).to_le_bytes()).chain([0, 0]).collect()),
            Value::Blob(b) => Arg::Blob(b.clone()),
            Value::Array(_) | Value::Struct(..) | Value::Repr(..) => {
                let mut b = Vec::new();
                pack(&mut b, v, 4)?;
                Arg::Blob(b)
            }
            Value::Double(_) => return Err(format!("{}: a double argument {arg_name}", call.sig.name)),
            v => Arg::Word(v.int().unwrap() as u32),
        })
    }

    fn this(&mut self, call: &Call) -> u32 {
        match call.args_in.first().and_then(|v| v.as_ref()) {
            Some(Value::Opaque(a)) => self.handle(*a),
            _ => 0,
        }
    }

    fn handle(&mut self, addr: u64) -> u32 {
        let n = self.objects.len() as u32 + 1;
        *self.objects.entry(addr).or_insert(n)
    }

    fn name(&mut self, name: &str) -> u32 {
        if let Some(&id) = self.names.get(name) {
            return id;
        }
        let id = self.names.len() as u32 + 1;
        self.names.insert(name.into(), id);
        self.out.push(NAME);
        self.out.extend_from_slice(&id.to_le_bytes());
        self.out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        self.out.extend_from_slice(name.as_bytes());
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

/// a member's width in the 32-bit C struct, where it is not 4 bytes
fn member_width(sname: &str, member: &str) -> Option<usize> {
    Some(match (sname, member) {
        ("D3DVERTEXELEMENT9", "Stream" | "Offset") => 2,
        ("D3DVERTEXELEMENT9", _) => 1,
        ("GUID" | "IID" | "_GUID", "Data2" | "Data3") => 2,
        ("GUID" | "IID" | "_GUID", "Data4") => 1,
        ("PALETTEENTRY" | "tagPALETTEENTRY", _) => 1,
        ("D3DGAMMARAMP" | "_D3DGAMMARAMP", _) => 2,
        _ => return None,
    })
}

/// a value as the bytes of its 32-bit C layout (`w`: the width of a plain integer)
pub fn pack(b: &mut Vec<u8>, v: &Value, w: usize) -> Result<(), String> {
    match v {
        Value::Null => b.extend(std::iter::repeat_n(0, w)),
        Value::Float(f) => b.extend_from_slice(&f.to_le_bytes()),
        Value::Double(f) => b.extend_from_slice(&f.to_le_bytes()),
        Value::Opaque(a) => b.extend_from_slice(&(*a as u32).to_le_bytes()),
        Value::Blob(x) => b.extend_from_slice(x),
        Value::Str(x) => b.extend(x.iter().copied().chain([0])),
        Value::WStr(x) => b.extend(x.iter().flat_map(|&c| (c as u16).to_le_bytes()).chain([0, 0])),
        Value::Repr(_, m) => pack(b, m, w)?,
        Value::Array(a) => {
            for x in a {
                pack(b, x, w)?;
            }
        }
        Value::Struct(sig, m) => {
            // (C alignment: a member starts at a multiple of its width; the struct is padded to
            // its widest member)
            let start = b.len();
            let mut align = 1;
            for (name, x) in sig.members.iter().zip(m) {
                let mw = member_width(&sig.name, name).unwrap_or(4);
                let a = if matches!(x, Value::Double(_)) { 8 } else { mw };
                while (b.len() - start) % a != 0 {
                    b.push(0);
                }
                align = align.max(a);
                pack(b, x, mw)?;
            }
            while (b.len() - start) % align != 0 {
                b.push(0);
            }
        }
        v => {
            let x = v.int().unwrap() as u64;
            b.extend_from_slice(&x.to_le_bytes()[..w]);
        }
    }
    Ok(())
}

fn write_arg(out: &mut Vec<u8>, a: &Arg) {
    let w32 = |out: &mut Vec<u8>, w: u32| out.extend_from_slice(&w.to_le_bytes());
    match a {
        Arg::Word(w) => {
            out.push(WORD);
            w32(out, *w);
        }
        Arg::Handle(h) => {
            out.push(HANDLE);
            w32(out, *h);
        }
        Arg::Blob(b) => {
            out.push(BLOB);
            w32(out, b.len() as u32);
            out.extend_from_slice(b);
        }
        Arg::OutObj(h) => {
            out.push(OUTOBJ);
            w32(out, *h);
        }
        Arg::Lock(k, key) => {
            out.push(*k);
            w32(out, *key);
        }
        Arg::Scratch => out.push(SCRATCH),
        Arg::Window => out.push(WINDOW),
    }
}

/// A replay stream read back (for the tests and `ippai trace dump`)
#[derive(Debug, PartialEq)]
pub enum Record {
    Call { no: u32, name: String, ret: Option<u32>, args: Vec<Arg> },
    Memcpy { handle: u32, key: u32, offset: u32, pitch: u32, slice: u32, data: Vec<u8> },
}

pub fn read(b: &[u8]) -> Result<Vec<Record>, String> {
    if b.get(..4) != Some(MAGIC) || b.get(4..8) != Some(&VERSION.to_le_bytes()) {
        return Err("not a replay stream of this version".into());
    }
    let mut i = 8;
    let u8_ = |i: &mut usize| -> Result<u8, String> {
        let v = *b.get(*i).ok_or("replay: past the end")?;
        *i += 1;
        Ok(v)
    };
    let u32_ = |i: &mut usize| -> Result<u32, String> {
        let v = u32::from_le_bytes(b.get(*i..*i + 4).ok_or("replay: past the end")?.try_into().unwrap());
        *i += 4;
        Ok(v)
    };
    let bytes = |i: &mut usize, n: usize| -> Result<Vec<u8>, String> {
        let v = b.get(*i..*i + n).ok_or("replay: past the end")?.to_vec();
        *i += n;
        Ok(v)
    };
    let mut names = HashMap::new();
    let mut out = vec![];
    while i < b.len() {
        match u8_(&mut i)? {
            NAME => {
                let id = u32_(&mut i)?;
                let n = u16::from_le_bytes(bytes(&mut i, 2)?.try_into().unwrap()) as usize;
                names.insert(id, String::from_utf8(bytes(&mut i, n)?).map_err(|e| e.to_string())?);
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
                        WORD => Arg::Word(u32_(&mut i)?),
                        HANDLE => Arg::Handle(u32_(&mut i)?),
                        BLOB => {
                            let n = u32_(&mut i)? as usize;
                            Arg::Blob(bytes(&mut i, n)?)
                        }
                        OUTOBJ => Arg::OutObj(u32_(&mut i)?),
                        k @ (LOCKBUF | LOCKRECT | LOCKBOX) => Arg::Lock(k, u32_(&mut i)?),
                        SCRATCH => Arg::Scratch,
                        WINDOW => Arg::Window,
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
            t => return Err(format!("replay: record {t}")),
        }
    }
    Ok(out)
}
