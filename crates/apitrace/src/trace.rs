//! apitrace's trace format (lib/trace/trace_format.hpp, trace_writer.cpp, trace_parser.cpp,
//! version 6): after the version, the semantic version and the properties, a stream of events.
//! A call is an ENTER (thread, function signature, the in arguments) and a LEAVE (its number,
//! the out arguments, the return value). Signatures (function, struct, enum, bitmask) are spelled
//! out the first time their id appears. Numbers are LEB128; a negative integer is SINT and its
//! magnitude.

use std::collections::HashMap;
use std::rc::Rc;

pub const VERSION: u64 = 6;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    SInt(i64),
    UInt(u64),
    Float(f32),
    Double(f64),
    Str(Vec<u8>),
    Blob(Vec<u8>),
    Enum(Rc<EnumSig>, i64),
    Bitmask(Rc<BitmaskSig>, u64),
    Array(Vec<Value>),
    Struct(Rc<StructSig>, Vec<Value>),
    Opaque(u64),
    /// (human-readable, machine): a D3D9 shader is (its disassembly, its tokens)
    Repr(Box<Value>, Box<Value>),
    WStr(Vec<u32>),
}

impl Value {
    /// an integer of any kind (enum, bitmask and bool too)
    pub fn int(&self) -> Option<i64> {
        Some(match self {
            Value::Bool(b) => *b as i64,
            Value::SInt(v) | Value::Enum(_, v) => *v,
            Value::UInt(v) | Value::Bitmask(_, v) => *v as i64,
            _ => return None,
        })
    }

    /// the value a pointer to one thing points to (apitrace writes it as an array of one)
    pub fn deref(&self) -> Option<&Value> {
        match self {
            Value::Array(v) if v.len() == 1 => Some(&v[0]),
            _ => None,
        }
    }

    /// a struct member by name
    pub fn member(&self, name: &str) -> Option<&Value> {
        match self {
            Value::Struct(sig, m) => sig.members.iter().position(|n| n == name).and_then(|i| m.get(i)),
            _ => None,
        }
    }
}

/// apitrace dump's way of showing a value: enums by name, structs as {Member = value}, blobs
/// by length
impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Value::Null => write!(f, "NULL"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::SInt(v) => write!(f, "{v}"),
            Value::UInt(v) => write!(f, "{v}"),
            Value::Float(v) => write!(f, "{v}f"),
            Value::Double(v) => write!(f, "{v}d"),
            Value::Str(s) => write!(f, "{:?}", String::from_utf8_lossy(s)),
            Value::WStr(s) => write!(f, "L{:?}", s.iter().map(|&c| char::from_u32(c).unwrap_or('?')).collect::<String>()),
            Value::Blob(b) => write!(f, "blob({})", b.len()),
            Value::Enum(sig, v) => match sig.values.iter().find(|(_, x)| x == v) {
                Some((n, _)) => write!(f, "{n}"),
                None => write!(f, "{v}"),
            },
            Value::Bitmask(sig, v) => {
                let mut rest = *v;
                let mut names = vec![];
                for (n, x) in &sig.flags {
                    if *x != 0 && rest & x == *x {
                        names.push(n.as_str());
                        rest &= !x;
                    }
                }
                if rest != 0 || names.is_empty() {
                    write!(f, "{}", names.iter().map(|s| s.to_string()).chain([format!("{rest:#x}")]).collect::<Vec<_>>().join("|"))
                } else {
                    write!(f, "{}", names.join("|"))
                }
            }
            Value::Array(a) => {
                write!(f, "[")?;
                for (i, x) in a.iter().enumerate() {
                    write!(f, "{}{x}", if i > 0 { ", " } else { "" })?;
                }
                write!(f, "]")
            }
            Value::Struct(sig, m) => {
                write!(f, "{}{{", sig.name)?;
                for (i, (n, x)) in sig.members.iter().zip(m).enumerate() {
                    write!(f, "{}{n} = {x}", if i > 0 { ", " } else { "" })?;
                }
                write!(f, "}}")
            }
            Value::Opaque(a) => write!(f, "{a:#x}"),
            Value::Repr(h, m) => match (&**h, &**m) {
                (Value::Str(s), m) => write!(f, "repr({} chars, {m})", s.len()),
                (h, m) => write!(f, "repr({h}, {m})"),
            },
        }
    }
}

impl std::fmt::Display for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{} {}{}(", self.no, self.sig.name, if self.fake { " (fake)" } else { "" })?;
        let n = self.sig.args.len().max(self.args_in.len()).max(self.args_out.len());
        for i in 0..n {
            let name = self.sig.args.get(i).map(|s| s.as_str()).unwrap_or("?");
            write!(f, "{}{name} = ", if i > 0 { ", " } else { "" })?;
            match (self.args_in.get(i).and_then(|v| v.as_ref()), self.args_out.get(i).and_then(|v| v.as_ref())) {
                (Some(a), Some(b)) if a == b => write!(f, "{a}")?,
                (Some(a), Some(b)) => write!(f, "{a} -> {b}")?,
                (Some(a), None) => write!(f, "{a}")?,
                (None, Some(b)) => write!(f, "-> {b}")?,
                (None, None) => write!(f, "?")?,
            }
        }
        write!(f, ")")?;
        if let Some(r) = &self.ret {
            write!(f, " = {r}")?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct FunctionSig {
    pub name: String,
    pub args: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub struct StructSig {
    pub name: String,
    pub members: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub struct EnumSig {
    pub values: Vec<(String, i64)>,
}

#[derive(Debug, PartialEq)]
pub struct BitmaskSig {
    pub flags: Vec<(String, u64)>,
}

/// A whole call: its in arguments from the ENTER, its out arguments from the LEAVE (an InOut
/// argument has both)
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub no: u64,
    pub thread: u64,
    pub sig: Rc<FunctionSig>,
    pub args_in: Vec<Option<Value>>,
    pub args_out: Vec<Option<Value>>,
    pub ret: Option<Value>,
    /// apitrace's own calls (memcpy of a locked region at Unlock, and the like)
    pub fake: bool,
}

impl Call {
    pub fn arg(&self, name: &str) -> Option<&Value> {
        let i = self.sig.args.iter().position(|a| a == name)?;
        self.args_in.get(i).and_then(|v| v.as_ref())
    }
    pub fn out(&self, name: &str) -> Option<&Value> {
        let i = self.sig.args.iter().position(|a| a == name)?;
        self.args_out.get(i).and_then(|v| v.as_ref())
    }
}

const EVENT_ENTER: u8 = 0;
const EVENT_LEAVE: u8 = 1;
const CALL_END: u8 = 0;
const CALL_ARG: u8 = 1;
const CALL_RET: u8 = 2;
const CALL_THREAD: u8 = 3;
const CALL_BACKTRACE: u8 = 4;
const CALL_FLAGS: u8 = 5;
const FLAG_FAKE: u64 = 1;

/// The decompressed event bytes of a .trace file (snappy, the format `apitrace trace` writes)
pub fn unpack(file: &[u8]) -> Result<Vec<u8>, String> {
    match unpack_cut(file)? {
        (_, true) => Err("trace: chunk past the end".into()),
        (out, false) => Ok(out),
    }
}

/// unpack() of a file that may end in the middle of a chunk (the program was killed while
/// apitrace wrote it): the whole chunks, and whether one was cut off. A whole chunk that does
/// not decompress is still an error.
pub fn unpack_cut(file: &[u8]) -> Result<(Vec<u8>, bool), String> {
    if file.get(..2) != Some(b"at") {
        return Err("not a snappy apitrace file (\"at\"); `apitrace repack` can make one".into());
    }
    let mut out = Vec::new();
    let mut i = 2;
    while i < file.len() {
        let Some(n) = file.get(i..i + 4) else { return Ok((out, true)) };
        let n = u32::from_le_bytes(n.try_into().unwrap()) as usize;
        i += 4;
        let Some(chunk) = file.get(i..i.saturating_add(n)) else { return Ok((out, true)) };
        out.extend(crate::snappy::decompress(chunk)?);
        i += n;
    }
    Ok((out, false))
}

/// Reads the calls of decompressed trace bytes, in the order they end
pub struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    pub version: u64,
    pub properties: Vec<(String, String)>,
    functions: HashMap<u64, Rc<FunctionSig>>,
    structs: HashMap<u64, Rc<StructSig>>,
    enums: HashMap<u64, Rc<EnumSig>>,
    bitmasks: HashMap<u64, Rc<BitmaskSig>>,
    pending: HashMap<u64, Call>,
    next_no: u64,
    /// the events may stop in the middle of one (a cut trace): running out of bytes ends the
    /// calls instead of failing
    pub cut: bool,
    /// a read ran out of bytes
    ended: bool,
}

type R<T> = Result<T, String>;

impl<'a> Parser<'a> {
    pub fn new(b: &'a [u8]) -> R<Self> {
        let mut p = Parser { b, i: 0, version: 0, properties: vec![], functions: HashMap::new(), structs: HashMap::new(), enums: HashMap::new(), bitmasks: HashMap::new(), pending: HashMap::new(), next_no: 0, cut: false, ended: false };
        p.version = p.uint()?;
        if p.version > VERSION {
            return Err(format!("trace format version {} (this reader knows up to {VERSION})", p.version));
        }
        if p.version >= 6 {
            let _semantic = p.uint()?;
            loop {
                let name = p.string()?;
                if name.is_empty() {
                    break;
                }
                let value = p.string()?;
                p.properties.push((String::from_utf8_lossy(&name).into(), String::from_utf8_lossy(&value).into()));
            }
        }
        Ok(p)
    }

    /// the next call that ended (None at the end; calls still open at the end are dropped, as
    /// apitrace does)
    pub fn next_call(&mut self) -> R<Option<Call>> {
        match self.next_call_() {
            // (a cut trace: the call being read when the bytes ran out is dropped, as are the
            // calls still open)
            Err(_) if self.cut && self.ended => {
                self.i = self.b.len();
                Ok(None)
            }
            r => r,
        }
    }

    fn next_call_(&mut self) -> R<Option<Call>> {
        while self.i < self.b.len() {
            match self.byte()? {
                EVENT_ENTER => {
                    let thread = if self.version >= 4 { self.uint()? } else { 0 };
                    let sig = self.function_sig()?;
                    let n = sig.args.len();
                    let mut call = Call { no: self.next_no, thread, sig, args_in: vec![None; n], args_out: vec![None; n], ret: None, fake: false };
                    self.next_no += 1;
                    self.details(&mut call, false)?;
                    self.pending.insert(call.no, call);
                }
                EVENT_LEAVE => {
                    let no = self.uint()?;
                    match self.pending.remove(&no) {
                        Some(mut call) => {
                            self.details(&mut call, true)?;
                            return Ok(Some(call));
                        }
                        None => {
                            // (a LEAVE of a call this reader never saw enter: skip its details)
                            let mut skip = Call { no, thread: 0, sig: Rc::new(FunctionSig { name: String::new(), args: vec![] }), args_in: vec![], args_out: vec![], ret: None, fake: false };
                            self.details(&mut skip, true)?;
                        }
                    }
                }
                e => return Err(format!("trace: unknown event {e} at byte {}", self.i - 1)),
            }
        }
        Ok(None)
    }

    fn details(&mut self, call: &mut Call, leave: bool) -> R<()> {
        loop {
            match self.byte()? {
                CALL_END => return Ok(()),
                CALL_ARG => {
                    let idx = self.uint()? as usize;
                    let v = self.value()?;
                    let args = if leave { &mut call.args_out } else { &mut call.args_in };
                    if idx >= args.len() {
                        args.resize(idx + 1, None);
                    }
                    args[idx] = Some(v);
                }
                CALL_RET => call.ret = Some(self.value()?),
                CALL_THREAD => call.thread = self.uint()?,
                CALL_BACKTRACE => {
                    let frames = self.uint()?;
                    for _ in 0..frames {
                        let _id = self.uint()?;
                        // (a frame seen before is just its id; a new one is details up to END)
                        loop {
                            match self.byte()? {
                                0 => break,
                                1..=3 => {
                                    self.string()?;
                                }
                                4 | 5 => {
                                    self.uint()?;
                                }
                                d => return Err(format!("trace: unknown backtrace detail {d}")),
                            }
                        }
                    }
                }
                CALL_FLAGS => call.fake |= self.uint()? & FLAG_FAKE != 0,
                d => return Err(format!("trace: unknown call detail {d} in {} at byte {}", call.sig.name, self.i - 1)),
            }
        }
    }

    fn function_sig(&mut self) -> R<Rc<FunctionSig>> {
        let id = self.uint()?;
        if let Some(s) = self.functions.get(&id) {
            return Ok(s.clone());
        }
        let name = String::from_utf8_lossy(&self.string()?).into_owned();
        let n = self.count()?;
        let args = (0..n).map(|_| self.string().map(|s| String::from_utf8_lossy(&s).into_owned())).collect::<R<_>>()?;
        let s = Rc::new(FunctionSig { name, args });
        self.functions.insert(id, s.clone());
        Ok(s)
    }

    fn value(&mut self) -> R<Value> {
        let t = self.byte()?;
        Ok(match t {
            0 => Value::Null,
            1 => Value::Bool(false),
            2 => Value::Bool(true),
            3 => Value::SInt((self.uint()? as i64).wrapping_neg()),
            4 => Value::UInt(self.uint()?),
            5 => Value::Float(f32::from_le_bytes(self.bytes(4)?.try_into().unwrap())),
            6 => Value::Double(f64::from_le_bytes(self.bytes(8)?.try_into().unwrap())),
            7 => Value::Str(self.string()?),
            8 => Value::Blob(self.string()?),
            9 => {
                let id = self.uint()?;
                let sig = match self.enums.get(&id) {
                    Some(s) => s.clone(),
                    None => {
                        let n = self.count()?;
                        let mut values = Vec::with_capacity(n);
                        for _ in 0..n {
                            let name = String::from_utf8_lossy(&self.string()?).into_owned();
                            let v = self.value()?.int().ok_or("trace: enum value is not an integer")?;
                            values.push((name, v));
                        }
                        let s = Rc::new(EnumSig { values });
                        self.enums.insert(id, s.clone());
                        s
                    }
                };
                let v = self.value()?.int().ok_or("trace: enum value is not an integer")?;
                Value::Enum(sig, v)
            }
            10 => {
                let id = self.uint()?;
                let sig = match self.bitmasks.get(&id) {
                    Some(s) => s.clone(),
                    None => {
                        let n = self.count()?;
                        let mut flags = Vec::with_capacity(n);
                        for _ in 0..n {
                            let name = String::from_utf8_lossy(&self.string()?).into_owned();
                            flags.push((name, self.uint()?));
                        }
                        let s = Rc::new(BitmaskSig { flags });
                        self.bitmasks.insert(id, s.clone());
                        s
                    }
                };
                Value::Bitmask(sig, self.uint()?)
            }
            11 => {
                let n = self.count()?;
                Value::Array((0..n).map(|_| self.value()).collect::<R<_>>()?)
            }
            12 => {
                let id = self.uint()?;
                let sig = match self.structs.get(&id) {
                    Some(s) => s.clone(),
                    None => {
                        let name = String::from_utf8_lossy(&self.string()?).into_owned();
                        let n = self.count()?;
                        let members = (0..n).map(|_| self.string().map(|s| String::from_utf8_lossy(&s).into_owned())).collect::<R<_>>()?;
                        let s = Rc::new(StructSig { name, members });
                        self.structs.insert(id, s.clone());
                        s
                    }
                };
                let m = (0..sig.members.len()).map(|_| self.value()).collect::<R<_>>()?;
                Value::Struct(sig, m)
            }
            13 => Value::Opaque(self.uint()?),
            14 => {
                let human = self.value()?;
                let machine = self.value()?;
                Value::Repr(Box::new(human), Box::new(machine))
            }
            15 => {
                let n = self.count()?;
                Value::WStr((0..n).map(|_| self.uint().map(|c| c as u32)).collect::<R<_>>()?)
            }
            t => return Err(format!("trace: unknown value type {t} at byte {}", self.i - 1)),
        })
    }

    fn byte(&mut self) -> R<u8> {
        let Some(&b) = self.b.get(self.i) else {
            self.ended = true;
            return Err("trace: unexpected end".into());
        };
        self.i += 1;
        Ok(b)
    }

    fn bytes(&mut self, n: usize) -> R<&'a [u8]> {
        let Some(s) = self.b.get(self.i..self.i.checked_add(n).ok_or("trace: length overflow")?) else {
            self.ended = true;
            return Err("trace: unexpected end".into());
        };
        self.i += n;
        Ok(s)
    }

    fn uint(&mut self) -> R<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            if shift < 64 {
                v |= ((b & 0x7f) as u64) << shift;
            }
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
            if shift > 70 {
                return Err("trace: number too long".into());
            }
        }
    }

    /// a count of things that follow (each at least a byte: a bigger count is a broken file)
    fn count(&mut self) -> R<usize> {
        let n = self.uint()?;
        if n > (self.b.len() - self.i) as u64 {
            self.ended = true;
            return Err(format!("trace: count {n} past the end"));
        }
        Ok(n as usize)
    }

    fn string(&mut self) -> R<Vec<u8>> {
        let n = self.count()?;
        Ok(self.bytes(n)?.to_vec())
    }
}

/// Writes the trace format (as apitrace's Writer does), for the tests: signatures spelled out the
/// first time.
#[cfg(test)]
pub mod writer {
    use super::*;
    use std::collections::HashSet;

    #[derive(Default)]
    pub struct Writer {
        pub b: Vec<u8>,
        seen: HashSet<(u8, usize)>,
        next_no: u64,
    }

    impl Writer {
        pub fn new(props: &[(&str, &str)]) -> Self {
            let mut w = Writer::default();
            w.uint(VERSION);
            w.uint(VERSION);
            for (k, v) in props {
                w.string(k.as_bytes());
                w.string(v.as_bytes());
            }
            w.uint(0);
            w
        }
        pub fn uint(&mut self, mut v: u64) {
            loop {
                let b = (v & 0x7f) as u8;
                v >>= 7;
                if v == 0 {
                    self.b.push(b);
                    return;
                }
                self.b.push(b | 0x80);
            }
        }
        pub fn string(&mut self, s: &[u8]) {
            self.uint(s.len() as u64);
            self.b.extend_from_slice(s);
        }
        fn first(&mut self, kind: u8, sig: usize) -> bool {
            self.seen.insert((kind, sig))
        }
        /// ENTER with these in arguments; returns the call number
        pub fn enter(&mut self, thread: u64, sig: &Rc<FunctionSig>, args: &[(usize, Value)]) -> u64 {
            self.b.push(EVENT_ENTER);
            self.uint(thread);
            let id = Rc::as_ptr(sig) as usize;
            self.uint(id as u64 & 0xffff_ffff);
            if self.first(0, id) {
                self.string(sig.name.as_bytes());
                self.uint(sig.args.len() as u64);
                for a in &sig.args {
                    self.string(a.as_bytes());
                }
            }
            for (i, v) in args {
                self.b.push(CALL_ARG);
                self.uint(*i as u64);
                self.value(v);
            }
            self.b.push(CALL_END);
            self.next_no += 1;
            self.next_no - 1
        }
        pub fn leave(&mut self, no: u64, args: &[(usize, Value)], ret: Option<&Value>, fake: bool) {
            self.b.push(EVENT_LEAVE);
            self.uint(no);
            for (i, v) in args {
                self.b.push(CALL_ARG);
                self.uint(*i as u64);
                self.value(v);
            }
            if let Some(r) = ret {
                self.b.push(CALL_RET);
                self.value(r);
            }
            if fake {
                self.b.push(CALL_FLAGS);
                self.uint(FLAG_FAKE);
            }
            self.b.push(CALL_END);
        }
        pub fn value(&mut self, v: &Value) {
            match v {
                Value::Null => self.b.push(0),
                Value::Bool(b) => self.b.push(1 + *b as u8),
                Value::SInt(x) if *x < 0 => {
                    self.b.push(3);
                    self.uint(x.unsigned_abs());
                }
                Value::SInt(x) => {
                    self.b.push(4);
                    self.uint(*x as u64);
                }
                Value::UInt(x) => {
                    self.b.push(4);
                    self.uint(*x);
                }
                Value::Float(f) => {
                    self.b.push(5);
                    self.b.extend_from_slice(&f.to_le_bytes());
                }
                Value::Double(f) => {
                    self.b.push(6);
                    self.b.extend_from_slice(&f.to_le_bytes());
                }
                Value::Str(s) => {
                    self.b.push(7);
                    self.string(s);
                }
                Value::Blob(s) => {
                    self.b.push(8);
                    self.string(s);
                }
                Value::Enum(sig, x) => {
                    self.b.push(9);
                    let id = Rc::as_ptr(sig) as usize;
                    self.uint(id as u64 & 0xffff_ffff);
                    if self.first(1, id) {
                        self.uint(sig.values.len() as u64);
                        for (n, x) in &sig.values {
                            self.string(n.as_bytes());
                            self.value(&Value::SInt(*x));
                        }
                    }
                    self.value(&Value::SInt(*x));
                }
                Value::Bitmask(sig, x) => {
                    self.b.push(10);
                    let id = Rc::as_ptr(sig) as usize;
                    self.uint(id as u64 & 0xffff_ffff);
                    if self.first(2, id) {
                        self.uint(sig.flags.len() as u64);
                        for (n, x) in &sig.flags {
                            self.string(n.as_bytes());
                            self.uint(*x);
                        }
                    }
                    self.uint(*x);
                }
                Value::Array(a) => {
                    self.b.push(11);
                    self.uint(a.len() as u64);
                    for x in a {
                        self.value(x);
                    }
                }
                Value::Struct(sig, m) => {
                    self.b.push(12);
                    let id = Rc::as_ptr(sig) as usize;
                    self.uint(id as u64 & 0xffff_ffff);
                    if self.first(3, id) {
                        self.string(sig.name.as_bytes());
                        self.uint(sig.members.len() as u64);
                        for n in &sig.members {
                            self.string(n.as_bytes());
                        }
                    }
                    for x in m {
                        self.value(x);
                    }
                }
                Value::Opaque(p) => {
                    self.b.push(13);
                    self.uint(*p);
                }
                Value::Repr(h, m) => {
                    self.b.push(14);
                    self.value(h);
                    self.value(m);
                }
                Value::WStr(s) => {
                    self.b.push(15);
                    self.uint(s.len() as u64);
                    for c in s {
                        self.uint(*c as u64);
                    }
                }
            }
        }
        /// the .trace file: "at" and snappy chunks of at most `chunk` bytes
        pub fn file(&self, chunk: usize) -> Vec<u8> {
            let mut f = b"at".to_vec();
            for c in self.b.chunks(chunk.max(1)) {
                let z = crate::snappy::compress(c);
                f.extend_from_slice(&(z.len() as u32).to_le_bytes());
                f.extend(z);
            }
            f
        }
    }
}
