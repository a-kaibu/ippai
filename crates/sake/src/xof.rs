//! The DirectX file format (.x files: d3dxof.dll's IDirectXFile, d3dxof.rs): the 16-byte header,
//! text and binary tokens, MSZIP-compressed bodies ("tzip", "bzip"), templates and data objects.
//! Written from the DirectX SDK's "X File Format Reference" and Wine's conformance tests
//! (dlls/d3dxof/tests), which run against Windows.
//!
//! A data object's data is what GetData hands out: its template's members packed with no padding
//! (WORD 2 bytes, DWORD 4, FLOAT 4, DOUBLE 8, CHAR 1), arrays and nested templates inline, strings
//! as pointers (filled in by d3dxof.rs; here their offsets and bytes).
//!
//! Separators, as Windows takes them (the conformance tests): every value not from a binary list
//! is followed by one or more semicolons and at most one comma after them, or by one comma alone
//! (`3;;;,` `0,` `5;`); nothing else (`0 1`, `1,,`, `1,;` and a last value with none are parse
//! errors). Values of a binary integer or float list take no separators at all, not even after
//! the last.

pub type Guid = [u8; 16];
pub type B = Vec<u8>;

const fn e(code: u32) -> u32 {
    0x8876_0000 | code
}
pub const BADOBJECT: u32 = e(850);
pub const BADVALUE: u32 = e(851);
pub const NOTFOUND: u32 = e(855);
pub const NOTDONEYET: u32 = e(856);
pub const FILENOTFOUND: u32 = e(857);
pub const RESOURCENOTFOUND: u32 = e(858);
pub const BADFILETYPE: u32 = e(861);
pub const BADFILEVERSION: u32 = e(862);
pub const BADFILEFLOATSIZE: u32 = e(863);
pub const BADFILECOMPRESSIONTYPE: u32 = e(864);
pub const BADFILE: u32 = e(865);
pub const PARSEERROR: u32 = e(866);
pub const BADDATAREFERENCE: u32 = e(869);
pub const NOMOREOBJECTS: u32 = e(871);

/// Bigger bodies, data or arrays than this are refused (BADFILE / PARSEERROR), not allocated.
const MAX_BODY: usize = 256 << 20;
const MAX_DEPTH: u32 = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prim {
    Word,
    Dword,
    Float,
    Double,
    Char,
    Uchar,
    Sword,
    Sdword,
    Void,
    Lpstr,
    Unicode,
    Cstring,
}

impl Prim {
    /// Its bytes in a data object's data (`ps`: the program's pointer size).
    pub fn size(self, ps: usize) -> usize {
        match self {
            Prim::Word | Prim::Sword => 2,
            Prim::Dword | Prim::Sdword | Prim::Float => 4,
            Prim::Double => 8,
            Prim::Char | Prim::Uchar => 1,
            Prim::Void => 0,
            Prim::Lpstr | Prim::Unicode | Prim::Cstring => ps,
        }
    }
    fn keyword(w: &[u8]) -> Option<Prim> {
        let k = w.to_ascii_uppercase();
        Some(match &k[..] {
            b"WORD" => Prim::Word,
            b"DWORD" => Prim::Dword,
            b"FLOAT" => Prim::Float,
            b"DOUBLE" => Prim::Double,
            b"CHAR" => Prim::Char,
            b"UCHAR" => Prim::Uchar,
            b"SWORD" => Prim::Sword,
            b"SDWORD" => Prim::Sdword,
            b"VOID" => Prim::Void,
            b"STRING" | b"LPSTR" => Prim::Lpstr,
            b"UNICODE" => Prim::Unicode,
            b"CSTRING" => Prim::Cstring,
            _ => return None,
        })
    }
    fn code(c: u16) -> Option<Prim> {
        Some(match c {
            0x28 => Prim::Word,
            0x29 => Prim::Dword,
            0x2a => Prim::Float,
            0x2b => Prim::Double,
            0x2c => Prim::Char,
            0x2d => Prim::Uchar,
            0x2e => Prim::Sword,
            0x2f => Prim::Sdword,
            0x30 => Prim::Void,
            0x31 => Prim::Lpstr,
            0x32 => Prim::Unicode,
            0x33 => Prim::Cstring,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Name(B),
    Str(B),
    /// a value; true: from a binary list (no separators follow)
    Int(u32, bool),
    Float(f64, bool),
    Guid(Guid),
    OBrace,
    CBrace,
    OParen,
    CParen,
    OBracket,
    CBracket,
    OAngle,
    CAngle,
    Dot,
    Comma,
    Semi,
    Template,
    Prim(Prim),
    Array,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Header {
    pub binary: bool,
    pub compressed: bool,
    pub float64: bool,
}

/// The 16-byte header "xof 0302txt 0032": magic, version 3.2 or 3.3, format, float size.
pub fn header(d: &[u8]) -> Result<Header, u32> {
    if d.len() < 16 || &d[..4] != b"xof " {
        return Err(BADFILETYPE);
    }
    if &d[4..6] != b"03" || !matches!(&d[6..8], b"02" | b"03") {
        return Err(BADFILEVERSION);
    }
    let (binary, compressed) = match &d[8..12] {
        b"txt " => (false, false),
        b"bin " => (true, false),
        b"tzip" => (false, true),
        b"bzip" => (true, true),
        _ => return Err(BADFILETYPE),
    };
    let float64 = match &d[12..16] {
        b"0032" => false,
        b"0064" => true,
        _ => return Err(BADFILEFLOATSIZE),
    };
    Ok(Header { binary, compressed, float64 })
}

/// A compressed body (after the header): the whole file's size (header included) as a DWORD, then
/// blocks of (uncompressed size, compressed size: WORDs) and "CK" + a deflate stream whose back
/// references reach into the blocks before it (MSZIP, as in cabinets).
pub fn unzip(d: &[u8]) -> Result<B, u32> {
    use miniz_oxide::inflate::core::{decompress, inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF, DecompressorOxide};
    use miniz_oxide::inflate::TINFLStatus;
    if d.len() < 4 {
        return Err(BADFILE);
    }
    let total = (u32::from_le_bytes([d[0], d[1], d[2], d[3]]) as usize).saturating_sub(16);
    if total > MAX_BODY {
        return Err(BADFILE);
    }
    let mut out: B = Vec::new();
    let mut p = 4;
    while out.len() < total && p + 4 <= d.len() {
        let usize_ = u16::from_le_bytes([d[p], d[p + 1]]) as usize;
        let csize = u16::from_le_bytes([d[p + 2], d[p + 3]]) as usize;
        p += 4;
        let Some(blk) = d.get(p..p + csize) else { return Err(BADFILE) };
        p += csize;
        if blk.len() < 2 || &blk[..2] != b"CK" {
            return Err(BADFILECOMPRESSIONTYPE);
        }
        let at = out.len();
        out.resize(at + usize_, 0);
        let mut r = DecompressorOxide::new();
        let (st, _, n) = decompress(&mut r, &blk[2..], &mut out, at, TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF);
        if !matches!(st, TINFLStatus::Done | TINFLStatus::HasMoreOutput) || n != usize_ {
            return Err(BADFILE);
        }
    }
    Ok(out)
}

/// The tokens of a whole file (header checked, body decompressed), and the error that stopped
/// them early if one did: that one comes when a reader gets that far (Windows reads lazily:
/// CreateEnumObject takes a file with garbage after its objects, GetNextDataObject fails there).
pub fn tokens(d: &[u8]) -> Result<(Vec<Tok>, Option<u32>), u32> {
    let h = header(d)?;
    let body = if h.compressed { unzip(&d[16..])? } else { d[16..].to_vec() };
    Ok(if h.binary { binary_tokens(&body, h.float64) } else { text_tokens(&body) })
}

fn is_sep(c: u8) -> bool {
    c <= b' ' || matches!(c, b'{' | b'}' | b'(' | b')' | b'[' | b']' | b'<' | b'>' | b',' | b';' | b'"')
}

fn hexval(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|v| v as u8)
}

/// "3D82AB43-62DA-11CF-AB39-0020AF71E433" (between the angle brackets) as a GUID's bytes.
pub fn parse_guid(s: &[u8]) -> Option<Guid> {
    if s.len() != 36 || s[8] != b'-' || s[13] != b'-' || s[18] != b'-' || s[23] != b'-' {
        return None;
    }
    let hex: Vec<u8> = s.iter().copied().filter(|&c| c != b'-').collect();
    if hex.len() != 32 {
        return None;
    }
    let mut v = [0u8; 16];
    for (i, b) in v.iter_mut().enumerate() {
        *b = hexval(hex[2 * i])? << 4 | hexval(hex[2 * i + 1])?;
    }
    // (Data1, Data2, Data3 are little-endian in memory, the last 8 bytes as written)
    v[..4].reverse();
    v[4..6].reverse();
    v[6..8].reverse();
    Some(v)
}

fn number(w: &[u8]) -> Option<Tok> {
    let s = std::str::from_utf8(w).ok()?;
    let body = s.strip_prefix(['-', '+']).unwrap_or(s);
    let first = body.bytes().next()?;
    if !(first.is_ascii_digit() || first == b'.' && body.len() > 1 && body.as_bytes()[1].is_ascii_digit()) {
        return None;
    }
    if body.bytes().all(|c| c.is_ascii_digit()) {
        let v: i128 = s.parse().ok().or_else(|| Some(if s.starts_with('-') { i128::MIN } else { i128::MAX }))?;
        return Some(Tok::Int(v.clamp(i64::MIN as i128, i64::MAX as i128) as i64 as u32, false));
    }
    if let Ok(f) = s.parse::<f64>() {
        if body.bytes().all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'-' | b'+')) {
            return Some(Tok::Float(f, false));
        }
    }
    // (MSVC's printf of a NaN or infinity: "-1.#IND00", "1.#QNAN0", "1.#INF00")
    if let Some(i) = s.find(".#") {
        if s[..i].bytes().all(|c| c.is_ascii_digit() || c == b'-' || c == b'+') {
            let neg = s.starts_with('-');
            return Some(Tok::Float(if s[i..].starts_with(".#INF") { if neg { f64::NEG_INFINITY } else { f64::INFINITY } } else { f64::NAN }, false));
        }
    }
    None
}

pub fn text_tokens(d: &[u8]) -> (Vec<Tok>, Option<u32>) {
    let mut t = Vec::new();
    let mut i = 0;
    while i < d.len() {
        let c = d[i];
        if c <= b' ' {
            i += 1;
            continue;
        }
        if c == b'#' || c == b'/' && d.get(i + 1) == Some(&b'/') {
            while i < d.len() && d[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let one = match c {
            b'{' => Some(Tok::OBrace),
            b'}' => Some(Tok::CBrace),
            b'(' => Some(Tok::OParen),
            b')' => Some(Tok::CParen),
            b'[' => Some(Tok::OBracket),
            b']' => Some(Tok::CBracket),
            b',' => Some(Tok::Comma),
            b';' => Some(Tok::Semi),
            b'>' => Some(Tok::CAngle),
            b'.' if !d.get(i + 1).is_some_and(|c| c.is_ascii_digit()) => Some(Tok::Dot),
            _ => None,
        };
        if let Some(k) = one {
            t.push(k);
            i += 1;
            continue;
        }
        if c == b'<' {
            if d.get(i + 37) == Some(&b'>') {
                if let Some(g) = parse_guid(&d[i + 1..i + 37]) {
                    t.push(Tok::Guid(g));
                    i += 38;
                    continue;
                }
            }
            t.push(Tok::OAngle);
            i += 1;
            continue;
        }
        if c == b'"' {
            let Some(n) = d[i + 1..].iter().position(|&c| c == b'"') else { return (t, Some(PARSEERROR)) };
            t.push(Tok::Str(d[i + 1..i + 1 + n].to_vec()));
            i += n + 2;
            continue;
        }
        let s = i;
        while i < d.len() && !is_sep(d[i]) && !(d[i] == b'/' && d.get(i + 1) == Some(&b'/')) {
            i += 1;
        }
        let w = &d[s..i];
        if w.eq_ignore_ascii_case(b"template") {
            t.push(Tok::Template);
        } else if w.eq_ignore_ascii_case(b"array") {
            t.push(Tok::Array);
        } else if let Some(p) = Prim::keyword(w) {
            t.push(Tok::Prim(p));
        } else if let Some(n) = number(w) {
            t.push(n);
        } else {
            t.push(Tok::Name(w.to_vec()));
        }
    }
    (t, None)
}

pub fn binary_tokens(d: &[u8], float64: bool) -> (Vec<Tok>, Option<u32>) {
    let mut t = Vec::new();
    let e = binary_into(d, float64, &mut t).err();
    (t, e)
}

fn binary_into(d: &[u8], float64: bool, t: &mut Vec<Tok>) -> Result<(), u32> {
    let mut i = 0;
    let dword = |i: usize| d.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or(PARSEERROR);
    let bytes = |i: usize, n: usize| d.get(i..i.checked_add(n)?).ok_or(()).ok();
    // (a last odd byte is no token: the end)
    while i + 2 <= d.len() {
        let c = u16::from_le_bytes([d[i], d[i + 1]]);
        i += 2;
        let k = match c {
            1 | 2 => {
                let n = dword(i)? as usize;
                let s = bytes(i + 4, n).ok_or(PARSEERROR)?.to_vec();
                i += 4 + n;
                if c == 1 { Tok::Name(s) } else { Tok::Str(s) }
            }
            3 => {
                let v = dword(i)?;
                i += 4;
                Tok::Int(v, false)
            }
            5 => {
                let g: Guid = bytes(i, 16).ok_or(PARSEERROR)?.try_into().unwrap();
                i += 16;
                Tok::Guid(g)
            }
            6 | 7 => {
                let n = dword(i)? as usize;
                let w = if c == 7 && float64 { 8 } else { 4 };
                let body = bytes(i + 4, n.checked_mul(w).ok_or(PARSEERROR)?).ok_or(PARSEERROR)?;
                i += 4 + n * w;
                for e in body.chunks_exact(w) {
                    t.push(match (c, w) {
                        (6, _) => Tok::Int(u32::from_le_bytes(e.try_into().unwrap()), true),
                        (_, 8) => Tok::Float(f64::from_le_bytes(e.try_into().unwrap()), true),
                        _ => Tok::Float(f32::from_le_bytes(e.try_into().unwrap()) as f64, true),
                    });
                }
                continue;
            }
            0x0a => Tok::OBrace,
            0x0b => Tok::CBrace,
            0x0c => Tok::OParen,
            0x0d => Tok::CParen,
            0x0e => Tok::OBracket,
            0x0f => Tok::CBracket,
            0x10 => Tok::OAngle,
            0x11 => Tok::CAngle,
            0x12 => Tok::Dot,
            0x13 => Tok::Comma,
            0x14 => Tok::Semi,
            0x1f => Tok::Template,
            0x34 => Tok::Array,
            _ => Tok::Prim(Prim::code(c).ok_or(PARSEERROR)?),
        };
        t.push(k);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Prim(Prim),
    Tpl(B),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Dim {
    Fixed(u32),
    Member(B),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub ty: Ty,
    pub name: Option<B>,
    pub dims: Vec<Dim>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Restrict {
    Closed,
    Open,
    Only(Vec<(B, Option<Guid>)>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    pub name: B,
    pub guid: Guid,
    pub members: Vec<Member>,
    pub restrict: Restrict,
}

/// The templates an IDirectXFile knows, by name (case matters).
#[derive(Clone, Debug, Default)]
pub struct Templates {
    pub list: Vec<Template>,
}

/// "indexColor": Windows takes it as a member type without a template of that name (the
/// standard MeshVertexColors once named it so; IndexedColor's layout: DWORD index, ColorRGBA).
fn index_color() -> &'static Template {
    static T: std::sync::OnceLock<Template> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let m = |ty, n: &str| Member { ty: Ty::Prim(ty), name: Some(n.as_bytes().to_vec()), dims: vec![] };
        Template {
            name: b"indexColor".to_vec(),
            guid: parse_guid(b"1630B820-7842-11CF-8F52-0040333594A3").unwrap(),
            members: vec![m(Prim::Dword, "index"), m(Prim::Float, "red"), m(Prim::Float, "green"), m(Prim::Float, "blue"), m(Prim::Float, "alpha")],
            restrict: Restrict::Closed,
        }
    })
}

impl Templates {
    pub fn find(&self, name: &[u8]) -> Option<&Template> {
        self.list.iter().find(|t| t.name == name).or_else(|| (name == b"indexColor").then(index_color))
    }
    /// A template read again (files repeat the ones they use) keeps the first.
    pub fn add(&mut self, t: Template) {
        if !self.list.iter().any(|o| o.name == t.name) {
            self.list.push(t);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Child {
    Obj(usize),
    Ref { name: Option<B>, guid: Option<Guid> },
}

/// A data object: its template, name, GUID, data (strings: offsets and bytes), top-level member
/// (name, offset, size) for GetData(member), children.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub tpl: B,
    pub tpl_guid: Guid,
    pub name: Option<B>,
    pub guid: Option<Guid>,
    pub data: B,
    pub strs: Vec<(usize, B)>,
    pub members: Vec<(B, usize, usize)>,
    pub children: Vec<Child>,
}

/// A file being read: its tokens, the objects read so far (nested ones too), the top-level ones.
#[derive(Debug, Default)]
pub struct XFile {
    toks: Vec<Tok>,
    pos: usize,
    pub nodes: Vec<Node>,
    pub top: Vec<usize>,
    ps: usize,
    budget: u64,
    /// a parse error stops the file: every later GetNextDataObject gives it
    failed: Option<u32>,
    /// the tokenizer's error past the last token
    tail: Option<u32>,
    /// top-level objects GetNextDataObject handed out (find reads ahead of it)
    handed: usize,
}

impl XFile {
    /// Header and tokens; then the templates before the first object, into `reg`.
    pub fn open(d: &[u8], reg: &mut Templates, ps: usize) -> Result<XFile, u32> {
        let (toks, tail) = tokens(d)?;
        let budget = 8 * toks.len() as u64 + 4096;
        let mut f = XFile { toks, ps, budget, tail, ..Default::default() };
        f.templates(reg)?;
        Ok(f)
    }

    fn templates(&mut self, reg: &mut Templates) -> Result<(), u32> {
        while self.peek() == Some(&Tok::Template) {
            let t = self.template(reg)?;
            reg.add(t);
        }
        Ok(())
    }

    /// The next top-level object (templates on the way are registered): None at the end.
    pub fn next_top(&mut self, reg: &mut Templates) -> Result<Option<usize>, u32> {
        if self.handed == self.top.len() {
            self.read_top(reg)?;
        }
        let n = self.top.get(self.handed).copied();
        self.handed += n.is_some() as usize;
        Ok(n)
    }

    /// One more top-level object read (None at the end).
    fn read_top(&mut self, reg: &mut Templates) -> Result<Option<usize>, u32> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        let r = (|| {
            self.templates(reg)?;
            if self.peek().is_none() {
                return self.tail.map_or(Ok(None), Err);
            }
            let n = self.object(reg, 0)?;
            self.top.push(n);
            Ok(Some(n))
        })();
        if let Err(e) = r {
            self.failed = Some(e);
        }
        r
    }

    /// The object `name` / `guid` names (any depth), reading on until it comes or the file ends.
    pub fn find(&mut self, reg: &mut Templates, name: Option<&[u8]>, guid: Option<&Guid>) -> Option<usize> {
        let hit = |n: &Node| name.is_none_or(|s| n.name.as_deref() == Some(s)) && guid.is_none_or(|g| n.guid.as_ref() == Some(g));
        if name.is_none() && guid.is_none() {
            return None;
        }
        loop {
            if let Some(i) = self.nodes.iter().position(hit) {
                return Some(i);
            }
            match self.read_top(reg) {
                Ok(Some(_)) => {}
                _ => return None,
            }
        }
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Result<Tok, u32> {
        let t = self.toks.get(self.pos).cloned().ok_or(self.tail.unwrap_or(PARSEERROR))?;
        self.pos += 1;
        Ok(t)
    }
    fn expect(&mut self, t: Tok) -> Result<(), u32> {
        if self.next()? == t { Ok(()) } else { Err(PARSEERROR) }
    }
    fn name(&mut self) -> Result<B, u32> {
        match self.next()? {
            Tok::Name(n) => Ok(n),
            _ => Err(PARSEERROR),
        }
    }
    fn opt_name(&mut self) -> Option<B> {
        if let Some(Tok::Name(n)) = self.peek() {
            let n = n.clone();
            self.pos += 1;
            return Some(n);
        }
        None
    }
    fn opt_guid(&mut self) -> Option<Guid> {
        if let Some(Tok::Guid(g)) = self.peek() {
            let g = *g;
            self.pos += 1;
            return Some(g);
        }
        None
    }

    /// template Name { <guid> members [restriction] }
    fn template(&mut self, reg: &Templates) -> Result<Template, u32> {
        self.expect(Tok::Template)?;
        let name = self.name()?;
        self.expect(Tok::OBrace)?;
        let guid = self.opt_guid().ok_or(PARSEERROR)?;
        let mut members: Vec<Member> = Vec::new();
        let mut restrict = Restrict::Closed;
        loop {
            match self.next()? {
                Tok::CBrace => break,
                Tok::OBracket => {
                    restrict = self.restriction()?;
                    self.expect(Tok::CBrace)?;
                    break;
                }
                Tok::Array => {
                    let ty = self.member_type(reg, &name)?;
                    let n = self.name()?;
                    let mut dims = Vec::new();
                    while self.peek() == Some(&Tok::OBracket) {
                        self.pos += 1;
                        dims.push(match self.next()? {
                            Tok::Int(v, _) => Dim::Fixed(v),
                            Tok::Name(m) => {
                                // (an earlier member of this template, an integer)
                                let ok = members.iter().any(|x| x.name.as_ref() == Some(&m) && x.dims.is_empty() && matches!(x.ty, Ty::Prim(Prim::Word | Prim::Dword | Prim::Sword | Prim::Sdword | Prim::Char | Prim::Uchar)));
                                if !ok {
                                    return Err(PARSEERROR);
                                }
                                Dim::Member(m)
                            }
                            _ => return Err(PARSEERROR),
                        });
                        self.expect(Tok::CBracket)?;
                    }
                    if dims.is_empty() {
                        return Err(PARSEERROR);
                    }
                    self.expect(Tok::Semi)?;
                    members.push(Member { ty, name: Some(n), dims });
                }
                t @ (Tok::Prim(_) | Tok::Name(_)) => {
                    self.pos -= 1;
                    let _ = t;
                    let ty = self.member_type(reg, &name)?;
                    let n = self.opt_name();
                    self.expect(Tok::Semi)?;
                    members.push(Member { ty, name: n, dims: vec![] });
                }
                _ => return Err(PARSEERROR),
            }
        }
        Ok(Template { name, guid, members, restrict })
    }

    fn member_type(&mut self, reg: &Templates, own: &[u8]) -> Result<Ty, u32> {
        match self.next()? {
            Tok::Prim(p) => Ok(Ty::Prim(p)),
            Tok::Name(t) if t != own && reg.find(&t).is_some() => Ok(Ty::Tpl(t)),
            _ => Err(PARSEERROR),
        }
    }

    /// after '[': "...]" (open) or "Name [<guid>], ...]"
    fn restriction(&mut self) -> Result<Restrict, u32> {
        if self.peek() == Some(&Tok::Dot) {
            for _ in 0..3 {
                self.expect(Tok::Dot)?;
            }
            self.expect(Tok::CBracket)?;
            return Ok(Restrict::Open);
        }
        let mut v = Vec::new();
        loop {
            let n = self.name()?;
            let g = self.opt_guid();
            v.push((n, g));
            match self.next()? {
                Tok::Comma => {}
                Tok::CBracket => return Ok(Restrict::Only(v)),
                _ => return Err(PARSEERROR),
            }
        }
    }

    /// Identifier [name] { [<guid>] members children }
    fn object(&mut self, reg: &Templates, depth: u32) -> Result<usize, u32> {
        if depth > MAX_DEPTH {
            return Err(PARSEERROR);
        }
        let id = self.name()?;
        let t = reg.find(&id).ok_or(PARSEERROR)?.clone();
        let name = self.opt_name();
        self.expect(Tok::OBrace)?;
        let guid = self.opt_guid();
        let mut node = Node { tpl: t.name.clone(), tpl_guid: t.guid, name, guid, data: Vec::new(), strs: Vec::new(), members: Vec::new(), children: Vec::new() };
        let mut top = Vec::new();
        self.members(reg, &t, &mut node, Some(&mut top), 0)?;
        node.members = top;
        let at = self.nodes.len();
        self.nodes.push(node);
        loop {
            match self.peek().ok_or(PARSEERROR)? {
                Tok::CBrace => {
                    self.pos += 1;
                    return Ok(at);
                }
                Tok::OBrace => {
                    self.pos += 1;
                    let n = self.opt_name();
                    let g = self.opt_guid();
                    if n.is_none() && g.is_none() {
                        return Err(PARSEERROR);
                    }
                    self.expect(Tok::CBrace)?;
                    self.nodes[at].children.push(Child::Ref { name: n, guid: g });
                }
                Tok::Name(c) => {
                    let allowed = match &t.restrict {
                        Restrict::Open => true,
                        Restrict::Closed => false,
                        Restrict::Only(v) => {
                            let cg = reg.find(c).map(|x| x.guid);
                            v.iter().any(|(n, g)| n == c || g.is_some() && *g == cg)
                        }
                    };
                    if !allowed {
                        return Err(PARSEERROR);
                    }
                    let k = self.object(reg, depth + 1)?;
                    self.nodes[at].children.push(Child::Obj(k));
                }
                _ => return Err(PARSEERROR),
            }
        }
    }

    fn spend(&mut self) -> Result<(), u32> {
        self.budget = self.budget.checked_sub(1).ok_or(PARSEERROR)?;
        Ok(())
    }

    /// `t`'s members into `node.data`; `top`: each member's (name, offset, size) at this level.
    fn members(&mut self, reg: &Templates, t: &Template, node: &mut Node, mut top: Option<&mut Vec<(B, usize, usize)>>, depth: u32) -> Result<(), u32> {
        if depth > MAX_DEPTH {
            return Err(PARSEERROR);
        }
        // (this level's members so far: name, offset, type, for array sizes)
        let mut seen: Vec<(Option<B>, usize, Ty)> = Vec::new();
        for m in &t.members {
            let mut count: u64 = 1;
            for d in &m.dims {
                let n = match d {
                    Dim::Fixed(n) => *n as u64,
                    Dim::Member(name) => {
                        let (_, off, ty) = seen.iter().rev().find(|s| s.0.as_ref() == Some(name)).ok_or(PARSEERROR)?;
                        let b = &node.data[*off..];
                        match ty {
                            Ty::Prim(Prim::Word | Prim::Sword) => u16::from_le_bytes([b[0], b[1]]) as u64,
                            Ty::Prim(Prim::Char | Prim::Uchar) => b[0] as u64,
                            _ => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64,
                        }
                    }
                };
                count = count.checked_mul(n).ok_or(PARSEERROR)?;
            }
            if count > MAX_BODY as u64 {
                return Err(PARSEERROR);
            }
            let off = node.data.len();
            let sub = match &m.ty {
                Ty::Tpl(n) => Some(reg.find(n).ok_or(PARSEERROR)?.clone()),
                Ty::Prim(_) => None,
            };
            for _ in 0..count {
                self.spend()?;
                match (&m.ty, &sub) {
                    (_, Some(s)) => self.members(reg, s, node, None, depth + 1)?,
                    (Ty::Prim(p), _) => self.value(*p, node)?,
                    _ => unreachable!(),
                }
                if node.data.len() > MAX_BODY {
                    return Err(PARSEERROR);
                }
            }
            seen.push((m.name.clone(), off, m.ty.clone()));
            if let Some(top) = top.as_deref_mut() {
                top.push((m.name.clone().unwrap_or_default(), off, node.data.len() - off));
            }
        }
        Ok(())
    }

    fn value(&mut self, p: Prim, node: &mut Node) -> Result<(), u32> {
        if p == Prim::Void {
            return Ok(());
        }
        let t = self.next()?;
        let list = match t {
            Tok::Int(_, l) | Tok::Float(_, l) => l,
            _ => false,
        };
        let int = |t: &Tok| match *t {
            Tok::Int(v, _) => Ok(v as u64),
            Tok::Float(f, _) => Ok(f as i64 as u64),
            _ => Err(PARSEERROR),
        };
        let flt = |t: &Tok| match *t {
            Tok::Int(v, _) => Ok(v as i32 as f64),
            Tok::Float(f, _) => Ok(f),
            _ => Err(PARSEERROR),
        };
        let d = &mut node.data;
        match p {
            Prim::Word | Prim::Sword => d.extend_from_slice(&(int(&t)? as u16).to_le_bytes()),
            Prim::Dword | Prim::Sdword => d.extend_from_slice(&(int(&t)? as u32).to_le_bytes()),
            Prim::Char | Prim::Uchar => d.push(int(&t)? as u8),
            Prim::Float => d.extend_from_slice(&(flt(&t)? as f32).to_le_bytes()),
            Prim::Double => d.extend_from_slice(&flt(&t)?.to_le_bytes()),
            Prim::Lpstr | Prim::Unicode | Prim::Cstring => {
                let Tok::Str(s) = t else { return Err(PARSEERROR) };
                node.strs.push((d.len(), s));
                d.resize(d.len() + self.ps, 0);
            }
            Prim::Void => {}
        }
        if !list {
            self.separators()?;
        }
        Ok(())
    }

    /// `;`+ then at most one `,`, or one `,`.
    fn separators(&mut self) -> Result<(), u32> {
        let mut any = false;
        while self.peek() == Some(&Tok::Semi) {
            self.pos += 1;
            any = true;
        }
        if self.peek() == Some(&Tok::Comma) {
            self.pos += 1;
            any = true;
        }
        if any { Ok(()) } else { Err(PARSEERROR) }
    }
}

/// RegisterTemplates: a file of templates only (all or none of them are added).
pub fn register(d: &[u8], reg: &mut Templates) -> Result<(), u32> {
    let mut r = reg.clone();
    let mut f = XFile::open(d, &mut r, 4)?;
    if f.peek().is_some() {
        return Err(PARSEERROR);
    }
    if let Some(e) = f.tail {
        return Err(e);
    }
    f.toks.clear();
    *reg = r;
    Ok(())
}

#[cfg(test)]
#[path = "xof_tests.rs"]
mod tests;
