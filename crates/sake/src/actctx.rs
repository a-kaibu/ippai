//! Activation contexts (side-by-side assemblies): CreateActCtx reads an application manifest
//! (a file, or an RT_MANIFEST resource of a module or a PE file), finds the assemblies it
//! depends on (private ones beside it, the system's Common Controls 6 built in), and the
//! context answers what Windows' does: QueryActCtxW (basic, detailed, assembly, file, run
//! level and compatibility information), QueryActCtxSettingsW, and FindActCtxSectionString /
//! Guid over the sections Windows lays out (DLL and window class redirection, COM servers,
//! interfaces, type libraries, ProgIDs, CLR surrogates, WinRT classes) in its binary formats.
//! ActivateActCtx and its ntdll forms keep each thread's frames in the TEB's
//! ActivationContextStackPointer, as programs read them.
//!
//! The manifest reader (`xml`, `parse_manifest`) and the section builders are pure: the
//! tests below run them on Wine's test manifests and on generated input.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem::{self, cstr, r16, r32, rp, w16, w32, wstr};
use crate::{HashMap, HashSet};
use std::sync::Mutex;

macro_rules! reg {
    ($dll:literal; $($f:ident),* $(,)?) => { $( implement($dll, stringify!($f), $f); )* };
}

const STATUS_SUCCESS: u32 = 0;
const STATUS_INVALID_PARAMETER: u32 = 0xC000_000D;
const STATUS_BUFFER_TOO_SMALL: u32 = 0xC000_0023;
const STATUS_NO_MEMORY: u32 = 0xC000_0017;
const STATUS_SXS_SECTION_NOT_FOUND: u32 = 0xC015_0001;
const STATUS_SXS_CANT_GEN_ACTCTX: u32 = 0xC015_0002;
const STATUS_SXS_KEY_NOT_FOUND: u32 = 0xC015_0008;
const STATUS_SXS_INVALID_DEACTIVATION: u32 = 0xC015_0010;

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_RESOURCE_TYPE_NOT_FOUND: u32 = 1813;
const ERROR_SXS_SECTION_NOT_FOUND: u32 = 14000;
const ERROR_SXS_CANT_GEN_ACTCTX: u32 = 14001;
const ERROR_SXS_KEY_NOT_FOUND: u32 = 14007;
const ERROR_SXS_INVALID_DEACTIVATION: u32 = 14010;

const INVALID_HANDLE_VALUE: u32 = u32::MAX;

/// The Win32 error of an activation context NTSTATUS.
fn win32(st: u32) -> u32 {
    match st {
        STATUS_SUCCESS => 0,
        STATUS_INVALID_PARAMETER => ERROR_INVALID_PARAMETER,
        STATUS_BUFFER_TOO_SMALL => ERROR_INSUFFICIENT_BUFFER,
        STATUS_NO_MEMORY => 8,
        STATUS_SXS_SECTION_NOT_FOUND => ERROR_SXS_SECTION_NOT_FOUND,
        STATUS_SXS_CANT_GEN_ACTCTX => ERROR_SXS_CANT_GEN_ACTCTX,
        STATUS_SXS_KEY_NOT_FOUND => ERROR_SXS_KEY_NOT_FOUND,
        STATUS_SXS_INVALID_DEACTIVATION => ERROR_SXS_INVALID_DEACTIVATION,
        s => crate::kern::win32(s),
    }
}

// ---------------------------------------------------------------------------------------------
// XML: what manifests use of it (namespaces, attributes, text, comments, CDATA, the
// declaration), strict about well-formedness as the Windows parser is.

pub mod xml {
    /// An element: its namespace URI ("" for none; "?prefix" for an undeclared prefix), local
    /// name, attributes (namespace declarations apart) and children.
    #[derive(Clone, Debug, Default, PartialEq)]
    pub struct Elem {
        pub ns: String,
        pub name: String,
        /// (namespace of a prefixed attribute or "", local name, value)
        pub attrs: Vec<(String, String, String)>,
        pub children: Vec<Elem>,
        /// the character data directly inside, concatenated
        pub text: String,
    }

    impl Elem {
        /// The value of the unprefixed attribute `name`.
        pub fn attr(&self, name: &str) -> Option<&str> {
            self.attrs.iter().find(|a| a.0.is_empty() && a.1 == name).map(|a| a.2.as_str())
        }
    }

    /// The text of a manifest file: UTF-8 (with or without its BOM), or UTF-16 of either byte
    /// order with its BOM. None: not text in these.
    pub fn decode(b: &[u8]) -> Option<String> {
        let utf16 = |be: bool| -> Option<String> {
            let rest = &b[2..];
            let units: Vec<u16> = rest.chunks(2).filter(|c| c.len() == 2).map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) }).collect();
            String::from_utf16(&units).ok()
        };
        match b {
            [0xff, 0xfe, ..] => utf16(false),
            [0xfe, 0xff, ..] => utf16(true),
            [0xef, 0xbb, 0xbf, rest @ ..] => std::str::from_utf8(rest).ok().map(str::to_string),
            _ => std::str::from_utf8(b).ok().map(str::to_string),
        }
    }

    struct P<'a> {
        s: &'a [char],
        i: usize,
    }

    fn is_name_char(c: char) -> bool {
        c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')
    }

    impl P<'_> {
        fn peek(&self) -> Option<char> {
            self.s.get(self.i).copied()
        }
        fn starts(&self, t: &str) -> bool {
            let mut i = self.i;
            for c in t.chars() {
                if self.s.get(i) != Some(&c) {
                    return false;
                }
                i += 1;
            }
            true
        }
        fn eat(&mut self, t: &str) -> bool {
            if self.starts(t) {
                self.i += t.chars().count();
                true
            } else {
                false
            }
        }
        fn ws(&mut self) -> bool {
            let at = self.i;
            while matches!(self.peek(), Some(' ' | '\t' | '\r' | '\n')) {
                self.i += 1;
            }
            self.i > at
        }
        fn name(&mut self) -> Option<String> {
            let at = self.i;
            match self.peek() {
                Some(c) if c.is_alphabetic() || c == '_' || c == ':' => {}
                _ => return None,
            }
            while self.peek().is_some_and(is_name_char) {
                self.i += 1;
            }
            Some(self.s[at..self.i].iter().collect())
        }
        /// Skips to past `end`; false at the end of input.
        fn skip_past(&mut self, end: &str) -> bool {
            while self.i < self.s.len() {
                if self.eat(end) {
                    return true;
                }
                self.i += 1;
            }
            false
        }
        /// Comments, processing instructions and white space between markup.
        fn misc(&mut self) -> Option<()> {
            loop {
                self.ws();
                if self.eat("<!--") {
                    if !self.skip_past("-->") {
                        return None;
                    }
                } else if self.starts("<?") {
                    self.i += 2;
                    if !self.skip_past("?>") {
                        return None;
                    }
                } else {
                    return Some(());
                }
            }
        }
    }

    /// Character references and the five predefined entities in `s`.
    fn unescape(s: &str) -> Option<String> {
        if !s.contains('&') {
            return Some(s.to_string());
        }
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(p) = rest.find('&') {
            out.push_str(&rest[..p]);
            let e = rest[p..].find(';')?;
            let ent = &rest[p + 1..p + e];
            let c = match ent {
                "lt" => '<',
                "gt" => '>',
                "amp" => '&',
                "quot" => '"',
                "apos" => '\'',
                _ if ent.starts_with("#x") || ent.starts_with("#X") => char::from_u32(u32::from_str_radix(&ent[2..], 16).ok()?)?,
                _ if ent.starts_with('#') => char::from_u32(ent[1..].parse().ok()?)?,
                _ => return None,
            };
            out.push(c);
            rest = &rest[p + e + 1..];
        }
        out.push_str(rest);
        Some(out)
    }

    struct Raw {
        qname: String,
        attrs: Vec<(String, String)>,
        children: Vec<Raw>,
        text: String,
    }

    fn element(p: &mut P, depth: u32) -> Option<Raw> {
        if depth > 256 || !p.eat("<") {
            return None;
        }
        let qname = p.name()?;
        let mut attrs: Vec<(String, String)> = Vec::new();
        loop {
            let had_ws = p.ws();
            if p.eat("/>") {
                return Some(Raw { qname, attrs, children: vec![], text: String::new() });
            }
            if p.eat(">") {
                break;
            }
            if !had_ws {
                return None;
            }
            let an = p.name()?;
            p.ws();
            if !p.eat("=") {
                return None;
            }
            p.ws();
            let q = p.peek()?;
            if q != '"' && q != '\'' {
                return None;
            }
            p.i += 1;
            let at = p.i;
            while p.peek()? != q {
                if p.peek()? == '<' {
                    return None;
                }
                p.i += 1;
            }
            let v: String = p.s[at..p.i].iter().collect();
            p.i += 1;
            if attrs.iter().any(|a| a.0 == an) {
                return None;
            }
            attrs.push((an, unescape(&v)?));
        }
        let mut children = Vec::new();
        let mut text = String::new();
        loop {
            if p.eat("</") {
                let end = p.name()?;
                p.ws();
                if end != qname || !p.eat(">") {
                    return None;
                }
                return Some(Raw { qname, attrs, children, text });
            }
            if p.eat("<!--") {
                if !p.skip_past("-->") {
                    return None;
                }
                continue;
            }
            if p.eat("<![CDATA[") {
                let at = p.i;
                if !p.skip_past("]]>") {
                    return None;
                }
                text.extend(&p.s[at..p.i - 3]);
                continue;
            }
            if p.starts("<?") {
                p.i += 2;
                if !p.skip_past("?>") {
                    return None;
                }
                continue;
            }
            if p.starts("<") {
                children.push(element(p, depth + 1)?);
                continue;
            }
            let at = p.i;
            while p.peek().is_some_and(|c| c != '<') {
                p.i += 1;
            }
            if p.i == at {
                return None;
            }
            let t: String = p.s[at..p.i].iter().collect();
            text.push_str(&unescape(&t)?);
        }
    }

    fn split(q: &str) -> (Option<&str>, &str) {
        match q.split_once(':') {
            Some((p, l)) => (Some(p), l),
            None => (None, q),
        }
    }

    fn resolve(r: Raw, scope: &[(String, String)]) -> Elem {
        let mut scope = scope.to_vec();
        for (n, v) in &r.attrs {
            if n == "xmlns" {
                scope.push((String::new(), v.clone()));
            } else if let Some(p) = n.strip_prefix("xmlns:") {
                scope.push((p.to_string(), v.clone()));
            }
        }
        let lookup = |p: &str| scope.iter().rev().find(|s| s.0 == p).map(|s| s.1.clone());
        let (prefix, local) = split(&r.qname);
        let ns = match prefix {
            None => lookup("").unwrap_or_default(),
            Some(p) => lookup(p).unwrap_or_else(|| format!("?{p}")),
        };
        let attrs = r
            .attrs
            .iter()
            .filter(|(n, _)| n != "xmlns" && !n.starts_with("xmlns:"))
            .map(|(n, v)| {
                let (prefix, local) = split(n);
                let ns = prefix.map(|p| lookup(p).unwrap_or_else(|| format!("?{p}"))).unwrap_or_default();
                (ns, local.to_string(), v.clone())
            })
            .collect();
        let name = local.to_string();
        let children = r.children.into_iter().map(|c| resolve(c, &scope)).collect();
        Elem { ns, name, attrs, children, text: r.text }
    }

    /// The root element of a document, or None when it is not well-formed XML (nothing but
    /// comments and white space may follow the root).
    pub fn parse(text: &str) -> Option<(Elem, Vec<(String, String)>)> {
        let chars: Vec<char> = text.chars().collect();
        let mut p = P { s: &chars, i: 0 };
        p.misc()?;
        let raw = element(&mut p, 0)?;
        p.misc()?;
        if p.i != chars.len() {
            return None;
        }
        // the root's own attributes as written (namespace declarations included): Windows
        // refuses an assembly element with attributes it does not know
        let root_attrs = raw.attrs.clone();
        Some((resolve(raw, &[]), root_attrs))
    }
}

// ---------------------------------------------------------------------------------------------
// manifests

const ASM_V1: &str = "urn:schemas-microsoft-com:asm.v1";
const ASM_V2: &str = "urn:schemas-microsoft-com:asm.v2";
const ASM_V3: &str = "urn:schemas-microsoft-com:asm.v3";
const COMPAT_V1: &str = "urn:schemas-microsoft-com:compatibility.v1";
const WINRT_V1: &str = "urn:schemas-microsoft-com:winrt.v1";
/// QueryActCtxSettingsW's namespaces (the first is a NULL namespace's)
const SETTINGS_NS: [&str; 5] = [
    "http://schemas.microsoft.com/SMI/2005/WindowsSettings",
    "http://schemas.microsoft.com/SMI/2011/WindowsSettings",
    "http://schemas.microsoft.com/SMI/2016/WindowsSettings",
    "http://schemas.microsoft.com/SMI/2017/WindowsSettings",
    "http://schemas.microsoft.com/SMI/2019/WindowsSettings",
];

fn is_asm(ns: &str) -> bool {
    matches!(ns, ASM_V1 | ASM_V2 | ASM_V3)
}

pub type Guid = [u8; 16];

/// "{12345678-1234-5678-1234-111122223333}" (braces optional) as a GUID's bytes.
pub fn parse_guid(s: &str) -> Option<Guid> {
    let s = s.trim();
    let s = s.strip_prefix('{').and_then(|s| s.strip_suffix('}')).unwrap_or(s);
    let p: Vec<&str> = s.split('-').collect();
    if p.len() != 5 || [8, 4, 4, 4, 12].iter().zip(&p).any(|(n, x)| x.len() != *n || !x.chars().all(|c| c.is_ascii_hexdigit())) {
        return None;
    }
    let mut g = [0u8; 16];
    g[..4].copy_from_slice(&u32::from_str_radix(p[0], 16).ok()?.to_le_bytes());
    g[4..6].copy_from_slice(&u16::from_str_radix(p[1], 16).ok()?.to_le_bytes());
    g[6..8].copy_from_slice(&u16::from_str_radix(p[2], 16).ok()?.to_le_bytes());
    let tail = format!("{}{}", p[3], p[4]);
    for i in 0..8 {
        g[8 + i] = u8::from_str_radix(&tail[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(g)
}

/// An assembly's identity, as its assemblyIdentity element gives it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ident {
    pub name: String,
    pub typ: Option<String>,
    pub version: Option<[u16; 4]>,
    pub arch: Option<String>,
    pub token: Option<String>,
    pub lang: Option<String>,
}

pub fn fmt_version(v: &[u16; 4]) -> String {
    format!("{}.{}.{}.{}", v[0], v[1], v[2], v[3])
}

/// "a.b.c.d", each a 16-bit number.
fn parse_version(s: &str) -> Option<[u16; 4]> {
    let p: Vec<&str> = s.split('.').collect();
    if p.len() != 4 {
        return None;
    }
    let mut v = [0u16; 4];
    for (i, x) in p.iter().enumerate() {
        if x.is_empty() || !x.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        v[i] = x.parse().ok()?;
    }
    Some(v)
}

impl Ident {
    /// The encoded identity QueryActCtxW gives: the name, then the attributes present in
    /// alphabetical order, `name,type="win32",version="1.0.0.0"`.
    pub fn encoded(&self) -> String {
        let mut s = self.name.clone();
        let mut add = |k: &str, v: &str| {
            s.push_str(&format!(",{k}=\"{v}\""));
        };
        if let Some(l) = &self.lang {
            add("language", l);
        }
        if let Some(a) = &self.arch {
            add("processorArchitecture", a);
        }
        if let Some(t) = &self.token {
            add("publicKeyToken", t);
        }
        if let Some(t) = &self.typ {
            add("type", t);
        }
        if let Some(v) = &self.version {
            add("version", &fmt_version(v));
        }
        s
    }
    /// The WinSxS-style directory name of the assembly ("x86_name_token_version_lang_hash").
    pub fn dir_name(&self) -> String {
        let v = self.version.map(|v| fmt_version(&v)).unwrap_or_else(|| "0.0.0.0".into());
        let lang = match self.lang.as_deref() {
            None | Some("*") | Some("") => "none".to_string(),
            Some(l) => l.to_ascii_lowercase(),
        };
        let h = hash_str(&self.encoded().encode_utf16().collect::<Vec<_>>());
        format!(
            "{}_{}_{}_{}_{}_{:08x}{:08x}",
            self.arch.as_deref().unwrap_or("none").to_ascii_lowercase(),
            self.name.to_ascii_lowercase(),
            self.token.as_deref().unwrap_or("none").to_ascii_lowercase(),
            v,
            lang,
            h,
            h.rotate_left(13) ^ 0x5bd1_e995
        )
    }
    /// Whether this definition answers the reference `r` (a dependency).
    pub fn satisfies(&self, r: &Ident) -> bool {
        let eq = |a: &Option<String>, b: &Option<String>| match (a, b) {
            (_, None) => true,
            (_, Some(b)) if b == "*" => true,
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            (None, Some(_)) => false,
        };
        let lang_ok = match r.lang.as_deref() {
            None | Some("*") => true,
            Some(l) => self.lang.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(l)),
        };
        self.name.eq_ignore_ascii_case(&r.name)
            && (r.version.is_none() || self.version == r.version)
            && eq(&self.arch, &r.arch)
            && eq(&self.token, &r.token)
            && lang_ok
    }
}

/// comClass / clrClass
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComClass {
    pub clsid: Guid,
    pub tlbid: Option<Guid>,
    pub progid: Option<String>,
    pub progids: Vec<String>,
    pub model: u32,
    /// miscStatus, miscStatusIcon, miscStatusContent, miscStatusThumbnail, miscStatusDocPrint
    pub misc: [Option<u32>; 5],
    pub description: Option<String>,
    /// clrClass: (name, runtimeVersion)
    pub clr: Option<(String, String)>,
}

/// comInterfaceProxyStub / comInterfaceExternalProxyStub
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IfacePs {
    pub iid: Guid,
    pub name: String,
    pub tlbid: Option<Guid>,
    pub ps32: Option<Guid>,
    pub base: Option<Guid>,
    pub nummethods: Option<u32>,
    pub external: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Typelib {
    pub tlbid: Guid,
    pub major: u16,
    pub minor: u16,
    pub help: String,
    pub flags: u16,
    pub lang: u16,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileEnt {
    pub name: String,
    /// (class, versioned)
    pub classes: Vec<(String, bool)>,
    pub com: Vec<ComClass>,
    pub ps: Vec<IfacePs>,
    pub typelibs: Vec<Typelib>,
    /// activatableClass: (name, threading model: both 0, sta 1, mta 2)
    pub winrt: Vec<(String, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Compat {
    Os(Guid),
    MaxVersion(u64),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dep {
    pub id: Ident,
    /// allowDelayedBinding / optional: a missing one does not fail the context
    pub optional: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Manifest {
    pub id: Ident,
    pub files: Vec<FileEnt>,
    pub deps: Vec<Dep>,
    /// comInterfaceExternalProxyStub
    pub ext_ps: Vec<IfacePs>,
    /// clrSurrogate: (clsid, name, runtimeVersion)
    pub surrogates: Vec<(Guid, String, String)>,
    pub clr: Vec<ComClass>,
    /// ACTCTX_REQUESTED_RUN_LEVEL and uiAccess
    pub run_level: u32,
    pub ui_access: bool,
    pub compat: Vec<Compat>,
    /// windowsSettings of the first application element that has them: (namespace, name, value)
    pub settings: Vec<(String, String, String)>,
}

fn threading_model(s: Option<&str>) -> Option<u32> {
    Some(match s.map(|s| s.to_ascii_lowercase()).as_deref() {
        None | Some("") => 0,
        Some("apartment") => 1,
        Some("free") => 2,
        Some("single") => 3,
        Some("both") => 4,
        Some("neutral") => 5,
        _ => return None,
    })
}

/// An OLEMISC flags list ("cantlinkinside,setclientsitefirst")
fn olemisc(s: &str) -> u32 {
    const NAMES: [&str; 22] = [
        "recomposeonresize", "onlyiconic", "insertnotreplace", "static", "cantlinkinside", "canlinkbyole1", "islinkobject", "insideout",
        "activatewhenvisible", "renderingisdeviceindependent", "invisibleatruntime", "alwaysrun", "actslikebutton", "actslikelabel", "nouiactivate",
        "alignable", "simpleframe", "setclientsitefirst", "imemode", "ignoreactivatewhenvisible", "wantstomenumerge", "supportsmultilevelundo",
    ];
    s.split(',').filter_map(|w| NAMES.iter().position(|n| n.eq_ignore_ascii_case(w.trim()))).fold(0, |a, i| a | 1 << i)
}

struct Reader {
    exec_levels: u32,
}

fn bad<T>() -> Result<T, ()> {
    Err(())
}

fn ident_of(e: &xml::Elem) -> Result<Ident, ()> {
    let mut id = Ident::default();
    for (ns, k, v) in &e.attrs {
        if !ns.is_empty() {
            continue;
        }
        match k.as_str() {
            "name" => id.name = v.clone(),
            "type" => id.typ = Some(v.clone()),
            "version" => id.version = Some(parse_version(v).ok_or(())?),
            "processorArchitecture" => id.arch = Some(v.clone()),
            "publicKeyToken" => id.token = Some(v.clone()),
            "language" => id.lang = Some(v.clone()),
            _ => {}
        }
    }
    Ok(id)
}

fn com_class(e: &xml::Elem, clr: bool) -> Result<ComClass, ()> {
    let mut c = ComClass { clsid: parse_guid(e.attr("clsid").ok_or(())?).ok_or(())?, ..Default::default() };
    c.tlbid = match e.attr("tlbid") {
        Some(t) => Some(parse_guid(t).ok_or(())?),
        None => None,
    };
    c.progid = e.attr("progid").map(str::to_string);
    c.description = e.attr("description").map(str::to_string);
    c.model = threading_model(e.attr("threadingModel")).ok_or(())?;
    for (i, k) in ["miscStatus", "miscStatusIcon", "miscStatusContent", "miscStatusThumbnail", "miscStatusDocPrint"].iter().enumerate() {
        c.misc[i] = e.attr(k).map(olemisc);
    }
    if clr {
        c.clr = Some((e.attr("name").unwrap_or_default().to_string(), e.attr("runtimeVersion").unwrap_or_default().to_string()));
    }
    for ch in &e.children {
        if is_asm(&ch.ns) && ch.name == "progid" {
            c.progids.push(ch.text.trim().to_string());
        }
    }
    Ok(c)
}

fn iface_ps(e: &xml::Elem, external: bool) -> Result<IfacePs, ()> {
    let g = |k: &str| -> Result<Option<Guid>, ()> {
        match e.attr(k) {
            Some(t) => parse_guid(t).map(Some).ok_or(()),
            None => Ok(None),
        }
    };
    Ok(IfacePs {
        iid: g("iid")?.ok_or(())?,
        name: e.attr("name").unwrap_or_default().to_string(),
        tlbid: g("tlbid")?,
        ps32: g("proxyStubClsid32")?,
        base: g("baseInterface")?,
        nummethods: match e.attr("numMethods") {
            Some(n) => Some(n.trim().parse().map_err(|_| ())?),
            None => None,
        },
        external,
    })
}

fn typelib(e: &xml::Elem) -> Result<Typelib, ()> {
    let mut t = Typelib { tlbid: parse_guid(e.attr("tlbid").ok_or(())?).ok_or(())?, ..Default::default() };
    let v = e.attr("version").ok_or(())?;
    let (a, b) = v.split_once('.').unwrap_or((v, "0"));
    t.major = a.trim().parse().map_err(|_| ())?;
    t.minor = b.trim().parse().map_err(|_| ())?;
    t.help = e.attr("helpdir").unwrap_or_default().to_string();
    if let Some(r) = e.attr("resourceid") {
        t.lang = u16::from_str_radix(r.trim(), 16).map_err(|_| ())?;
    }
    if let Some(f) = e.attr("flags") {
        for w in f.split(',') {
            t.flags |= match w.trim().to_ascii_lowercase().as_str() {
                "restricted" => 1,
                "control" => 2,
                "hidden" => 4,
                "hasdiskimage" => 8,
                "" => 0,
                _ => return bad(),
            };
        }
    }
    Ok(t)
}

impl Reader {
    fn file(&mut self, e: &xml::Elem) -> Result<FileEnt, ()> {
        let mut f = FileEnt { name: e.attr("name").filter(|n| !n.is_empty()).ok_or(())?.to_string(), ..Default::default() };
        if let Some(h) = e.attr("hash") {
            if h.len() != 40 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
                return bad();
            }
        }
        for ch in &e.children {
            if ch.ns == WINRT_V1 && ch.name == "activatableClass" {
                let m = match ch.attr("threadingModel").map(|s| s.to_ascii_lowercase()).as_deref() {
                    Some("both") => 0,
                    Some("sta") => 1,
                    Some("mta") => 2,
                    _ => return bad(),
                };
                f.winrt.push((ch.attr("name").ok_or(())?.to_string(), m));
                continue;
            }
            if !is_asm(&ch.ns) {
                continue;
            }
            match ch.name.as_str() {
                "windowClass" => {
                    let versioned = match ch.attr("versioned") {
                        None => true,
                        Some(v) if v.eq_ignore_ascii_case("yes") => true,
                        Some(v) if v.eq_ignore_ascii_case("no") => false,
                        _ => return bad(),
                    };
                    f.classes.push((ch.text.trim().to_string(), versioned));
                }
                "comClass" => f.com.push(com_class(ch, false)?),
                "comInterfaceProxyStub" => f.ps.push(iface_ps(ch, false)?),
                "typelib" => f.typelibs.push(typelib(ch)?),
                // (an activatableClass outside the WinRT namespace, and anything else unknown)
                _ => return bad(),
            }
        }
        Ok(f)
    }

    fn trust(&mut self, e: &xml::Elem, m: &mut Manifest) -> Result<(), ()> {
        for ch in &e.children {
            if !is_asm(&ch.ns) {
                continue;
            }
            match ch.name.as_str() {
                "security" | "requestedPrivileges" => self.trust(ch, m)?,
                "requestedExecutionLevel" => {
                    self.exec_levels += 1;
                    if self.exec_levels > 1 {
                        return bad();
                    }
                    m.run_level = match ch.attr("level").map(|s| s.to_ascii_lowercase()).as_deref() {
                        None => 0,
                        Some("asinvoker") => 1,
                        Some("highestavailable") => 2,
                        Some("requireadministrator") => 3,
                        _ => return bad(),
                    };
                    m.ui_access = match ch.attr("uiAccess").map(|s| s.to_ascii_lowercase()).as_deref() {
                        None | Some("false") => false,
                        Some("true") => true,
                        _ => return bad(),
                    };
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn compat(&mut self, e: &xml::Elem, m: &mut Manifest) {
        for app in e.children.iter().filter(|c| c.ns == COMPAT_V1 && c.name == "application") {
            for ch in app.children.iter().filter(|c| c.ns == COMPAT_V1) {
                match ch.name.as_str() {
                    "supportedOS" => {
                        if let Some(g) = ch.attr("Id").and_then(parse_guid) {
                            m.compat.push(Compat::Os(g));
                        }
                    }
                    "maxversiontested" => {
                        if let Some(id) = ch.attr("Id") {
                            let mut v = 0u64;
                            let parts: Vec<&str> = id.split('.').collect();
                            if parts.len() <= 4 && parts.iter().all(|p| p.parse::<u16>().is_ok()) {
                                for (i, p) in parts.iter().enumerate() {
                                    v |= (p.parse::<u16>().unwrap() as u64) << (48 - 16 * i);
                                }
                                m.compat.push(Compat::MaxVersion(v));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn application(&mut self, e: &xml::Elem, m: &mut Manifest) {
        if !m.settings.is_empty() || e.ns != ASM_V3 {
            return;
        }
        for ws in e.children.iter().filter(|c| c.ns == ASM_V3 && c.name == "windowsSettings") {
            for s in &ws.children {
                m.settings.push((s.ns.clone(), s.name.clone(), s.text.trim().to_string()));
            }
        }
    }

    fn assembly(&mut self, root: &xml::Elem, root_attrs: &[(String, String)]) -> Result<Manifest, ()> {
        if !is_asm(&root.ns) || root.name != "assembly" {
            return bad();
        }
        let mut version = None;
        for (k, v) in root_attrs {
            match k.as_str() {
                "manifestVersion" => version = Some(v.as_str()),
                k if k == "xmlns" || k.starts_with("xmlns:") => {}
                _ => return bad(),
            }
        }
        if version != Some("1.0") {
            return bad();
        }
        let mut m = Manifest::default();
        let mut have_id = false;
        for ch in &root.children {
            if ch.ns == COMPAT_V1 && ch.name == "compatibility" {
                self.compat(ch, &mut m);
                continue;
            }
            if !is_asm(&ch.ns) {
                continue;
            }
            match ch.name.as_str() {
                "assemblyIdentity" if !have_id => {
                    m.id = ident_of(ch)?;
                    have_id = true;
                }
                "description" | "noInherit" | "noInheritable" | "configuration" => {}
                "file" => {
                    let f = self.file(ch)?;
                    m.files.push(f);
                }
                "dependency" => {
                    for da in ch.children.iter().filter(|c| is_asm(&c.ns)) {
                        if da.name != "dependentAssembly" {
                            return bad();
                        }
                        let optional = ch.attr("optional").is_some_and(|v| v.eq_ignore_ascii_case("yes"))
                            || da.attr("allowDelayedBinding").is_some_and(|v| v.eq_ignore_ascii_case("true"));
                        let Some(idel) = da.children.iter().find(|c| is_asm(&c.ns) && c.name == "assemblyIdentity") else { return bad() };
                        m.deps.push(Dep { id: ident_of(idel)?, optional });
                    }
                }
                "comInterfaceExternalProxyStub" => m.ext_ps.push(iface_ps(ch, true)?),
                "clrSurrogate" => m.surrogates.push((
                    parse_guid(ch.attr("clsid").ok_or(())?).ok_or(())?,
                    ch.attr("name").unwrap_or_default().to_string(),
                    ch.attr("runtimeVersion").unwrap_or_default().to_string(),
                )),
                "clrClass" => m.clr.push(com_class(ch, true)?),
                "trustInfo" => self.trust(ch, &mut m)?,
                "application" => self.application(ch, &mut m),
                _ => return bad(),
            }
        }
        Ok(m)
    }
}

/// A manifest file's assembly, or None when Windows would refuse it
/// (ERROR_SXS_CANT_GEN_ACTCTX).
pub fn parse_manifest(bytes: &[u8]) -> Option<Manifest> {
    let text = xml::decode(bytes)?;
    let (root, attrs) = xml::parse(&text)?;
    Reader { exec_levels: 0 }.assembly(&root, &attrs).ok()
}

// ---------------------------------------------------------------------------------------------
// sections: Windows' string ("SsHd") and GUID ("GsHd") section formats

pub const STRSECTION_MAGIC: u32 = 0x6448_7353;
pub const GUIDSECTION_MAGIC: u32 = 0x6448_7347;
const STR_HEADER: u32 = 44;
const GUID_HEADER: u32 = 40;

/// RtlHashUnicodeString(case-insensitive, HASH_STRING_ALGORITHM_X65599)
pub fn hash_str(s: &[u16]) -> u32 {
    s.iter().fold(0u32, |h, &c| h.wrapping_mul(65599).wrapping_add(crate::text::upper16(c) as u32))
}

fn eq_nocase(a: &[u16], b: &[u16]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(&x, &y)| crate::text::upper16(x) == crate::text::upper16(y))
}

#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Str(Vec<u16>),
    Guid(Guid),
}

/// The data of one or more keys (a COM class is found by its CLSID and its alias): built
/// knowing where it lands (`data` and the global area's section offsets).
pub struct Ent {
    pub keys: Vec<Key>,
    pub roster: u32,
    pub data: Box<dyn Fn(u32, u32) -> Vec<u8>>,
}

fn pad4(v: &mut Vec<u8>) {
    while v.len() % 4 != 0 {
        v.push(0);
    }
}
fn p32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn p16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn pw(v: &mut Vec<u8>, s: &[u16]) {
    for &c in s {
        p16(v, c);
    }
    p16(v, 0);
}
fn get32(v: &[u8], at: u32) -> Option<u32> {
    let at = at as usize;
    Some(u32::from_le_bytes(v.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
pub fn w16s(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// A section: a string section when the keys are strings, else a GUID one. `global` is the
/// section's global data (module names, ProgID aliases), placed last.
pub fn build_section(guid: bool, ents: &[Ent], global: &[u8]) -> Vec<u8> {
    let n: u32 = ents.iter().map(|e| e.keys.len() as u32).sum();
    let (hdr, isz) = if guid { (GUID_HEADER, 28) } else { (STR_HEADER, 24) };
    let index_off = hdr;
    let mut body = Vec::new();
    let body_off = index_off + isz * n;
    // pass 1: sizes (data does not change size with where it lands)
    let mut sizes = Vec::new();
    let mut total = body_off;
    for e in ents {
        let names: u32 = if guid { 0 } else { e.keys.iter().map(|k| if let Key::Str(s) = k { (2 * s.len() as u32 + 2).next_multiple_of(4) } else { 0 }).sum() };
        let d = (e.data)(0, 0).len() as u32;
        sizes.push(d);
        total += names + d.next_multiple_of(4);
    }
    let global_off = total;
    let mut index = Vec::new();
    for (e, &dsz) in ents.iter().zip(&sizes) {
        let mut name_offs = Vec::new();
        if !guid {
            for k in &e.keys {
                if let Key::Str(s) = k {
                    name_offs.push(body_off + body.len() as u32);
                    pw(&mut body, s);
                    pad4(&mut body);
                }
            }
        }
        let data_off = body_off + body.len() as u32;
        let d = (e.data)(data_off, global_off);
        debug_assert_eq!(d.len() as u32, dsz);
        body.extend_from_slice(&d);
        pad4(&mut body);
        for (i, k) in e.keys.iter().enumerate() {
            match k {
                Key::Str(s) => {
                    p32(&mut index, hash_str(s));
                    p32(&mut index, name_offs[i]);
                    p32(&mut index, 2 * s.len() as u32);
                    p32(&mut index, data_off);
                    p32(&mut index, dsz);
                    p32(&mut index, e.roster);
                }
                Key::Guid(g) => {
                    index.extend_from_slice(g);
                    p32(&mut index, data_off);
                    p32(&mut index, dsz);
                    p32(&mut index, e.roster);
                }
            }
        }
    }
    let mut out = Vec::new();
    if guid {
        for x in [GUIDSECTION_MAGIC, GUID_HEADER, 1, 0, 0, n, index_off, 0, global_off, global.len() as u32] {
            p32(&mut out, x);
        }
    } else {
        for x in [STRSECTION_MAGIC, STR_HEADER, 1, 0, 0, n, index_off, 1, 0, global_off, global.len() as u32] {
            p32(&mut out, x);
        }
    }
    out.extend_from_slice(&index);
    out.extend_from_slice(&body);
    debug_assert_eq!(out.len() as u32, global_off);
    out.extend_from_slice(global);
    pad4(&mut out);
    out
}

/// A key's (data offset, data length, roster index) in a built section.
pub fn section_find(sec: &[u8], key: &Key) -> Option<(u32, u32, u32)> {
    let magic = get32(sec, 0)?;
    let count = get32(sec, if magic == GUIDSECTION_MAGIC { 20 } else { 20 })?;
    let index = get32(sec, 24)?;
    match key {
        Key::Str(s) if magic == STRSECTION_MAGIC => {
            let h = hash_str(s);
            for i in 0..count {
                let e = index.checked_add(i.checked_mul(24)?)?;
                if get32(sec, e)? != h {
                    continue;
                }
                let (no, nl) = (get32(sec, e + 4)?, get32(sec, e + 8)?);
                let name: Vec<u16> = sec.get(no as usize..no.checked_add(nl)? as usize)?.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                if eq_nocase(&name, s) {
                    return Some((get32(sec, e + 12)?, get32(sec, e + 16)?, get32(sec, e + 20)?));
                }
            }
            None
        }
        Key::Guid(g) if magic == GUIDSECTION_MAGIC => {
            for i in 0..count {
                let e = index.checked_add(i.checked_mul(28)?)? as usize;
                if sec.get(e..e + 16)? == g {
                    let e = e as u32;
                    return Some((get32(sec, e + 16)?, get32(sec, e + 20)?, get32(sec, e + 24)?));
                }
            }
            None
        }
        _ => None,
    }
}

/// (global data offset, length) of a section
pub fn section_global(sec: &[u8]) -> (u32, u32) {
    if get32(sec, 0) == Some(GUIDSECTION_MAGIC) {
        (get32(sec, 32).unwrap_or(0), get32(sec, 36).unwrap_or(0))
    } else {
        (get32(sec, 36).unwrap_or(0), get32(sec, 40).unwrap_or(0))
    }
}

pub const SEC_ASSEMBLY_INFORMATION: u32 = 1;
pub const SEC_DLL_REDIRECTION: u32 = 2;
pub const SEC_WINDOW_CLASS_REDIRECTION: u32 = 3;
pub const SEC_COM_SERVER_REDIRECTION: u32 = 4;
pub const SEC_COM_INTERFACE_REDIRECTION: u32 = 5;
pub const SEC_COM_TYPE_LIBRARY_REDIRECTION: u32 = 6;
pub const SEC_COM_PROGID_REDIRECTION: u32 = 7;
pub const SEC_CLR_SURROGATES: u32 = 9;
pub const SEC_WINRT_ACTIVATABLE_CLASSES: u32 = 12;

fn is_guid_section(id: u32) -> Option<bool> {
    match id {
        SEC_ASSEMBLY_INFORMATION | SEC_DLL_REDIRECTION | SEC_WINDOW_CLASS_REDIRECTION | SEC_COM_PROGID_REDIRECTION | SEC_WINRT_ACTIVATABLE_CLASSES => Some(false),
        SEC_COM_SERVER_REDIRECTION | SEC_COM_INTERFACE_REDIRECTION | SEC_COM_TYPE_LIBRARY_REDIRECTION | SEC_CLR_SURROGATES => Some(true),
        _ => None,
    }
}

/// The alias GUID of COM class `n` of a context (what its ProgIDs point at)
fn alias_guid(n: u32) -> Guid {
    let mut g = *b"\x9a\x5e\x1c\x4bsake\xa1as\x00\x00\x00\x00\x00";
    g[12..].copy_from_slice(&n.to_le_bytes());
    g
}

/// Strings laid out one after the other (NUL-ended, 4-aligned): the bytes and each one's offset.
struct Names {
    bytes: Vec<u8>,
    at: HashMap<Vec<u16>, u32>,
}
impl Names {
    fn new() -> Self {
        Names { bytes: Vec::new(), at: HashMap::default() }
    }
    fn add(&mut self, s: &[u16]) -> u32 {
        if let Some(&o) = self.at.get(s) {
            return o;
        }
        let o = self.bytes.len() as u32;
        pw(&mut self.bytes, s);
        pad4(&mut self.bytes);
        self.at.insert(s.to_vec(), o);
        o
    }
}

/// Section `id` of a context with assemblies `asms` (roster index = position + 1).
pub fn make_section(id: u32, asms: &[Manifest]) -> Option<Vec<u8>> {
    let guid = is_guid_section(id)?;
    let mut ents: Vec<Ent> = Vec::new();
    let mut names = Names::new();
    match id {
        SEC_DLL_REDIRECTION => {
            for (i, m) in asms.iter().enumerate() {
                for f in &m.files {
                    ents.push(Ent {
                        keys: vec![Key::Str(w16s(&f.name))],
                        roster: i as u32 + 1,
                        data: Box::new(|_, _| {
                            let mut v = Vec::new();
                            for x in [20, 2, 0, 0, 0] {
                                p32(&mut v, x);
                            }
                            v
                        }),
                    });
                }
            }
        }
        SEC_WINDOW_CLASS_REDIRECTION => {
            for (i, m) in asms.iter().enumerate() {
                for f in &m.files {
                    for (cls, versioned) in &f.classes {
                        let full = match (versioned, m.id.version) {
                            (true, Some(v)) => w16s(&format!("{}!{cls}", fmt_version(&v))),
                            _ => w16s(cls),
                        };
                        let module = w16s(&f.name);
                        ents.push(Ent {
                            keys: vec![Key::Str(w16s(cls))],
                            roster: i as u32 + 1,
                            data: Box::new(move |off, _| {
                                let mut v = Vec::new();
                                let nl = 2 * full.len() as u32;
                                for x in [24, 0, nl, 24, 2 * module.len() as u32, off + 24 + nl + 2] {
                                    p32(&mut v, x);
                                }
                                pw(&mut v, &full);
                                pw(&mut v, &module);
                                v
                            }),
                        });
                    }
                }
            }
        }
        SEC_COM_SERVER_REDIRECTION | SEC_COM_PROGID_REDIRECTION => {
            // every class of every assembly, numbered for its alias
            let mut n = 0u32;
            let mut aliases = Vec::new();
            for (i, m) in asms.iter().enumerate() {
                let classes = m.files.iter().flat_map(|f| f.com.iter().map(move |c| (c, Some(f.name.as_str())))).chain(m.clr.iter().map(|c| (c, None)));
                for (c, file) in classes {
                    n += 1;
                    let alias = alias_guid(n);
                    let roster = i as u32 + 1;
                    if id == SEC_COM_PROGID_REDIRECTION {
                        let at = aliases.len() as u32;
                        aliases.extend_from_slice(&alias);
                        for p in c.progid.iter().chain(&c.progids) {
                            ents.push(Ent {
                                keys: vec![Key::Str(w16s(p))],
                                roster,
                                data: Box::new(move |_, g| {
                                    let mut v = Vec::new();
                                    for x in [12, 0, g + at] {
                                        p32(&mut v, x);
                                    }
                                    v
                                }),
                            });
                        }
                        continue;
                    }
                    let module = match file {
                        Some(f) => w16s(f),
                        None => w16s("MSCOREE.DLL"),
                    };
                    let name_at = names.add(&module);
                    let mscoree_at = names.add(&w16s("mscoree.dll"));
                    let c = c.clone();
                    let module_len = 2 * module.len() as u32;
                    ents.push(Ent {
                        keys: vec![Key::Guid(c.clsid), Key::Guid(alias)],
                        roster,
                        data: Box::new(move |_, g| {
                            let mut clr = Vec::new();
                            if let Some((name, ver)) = &c.clr {
                                let (name, ver) = (w16s(name), w16s(ver));
                                let nl = 2 * name.len() as u32;
                                let vl = 2 * ver.len() as u32;
                                for x in [44, 0, 2, 22, g + mscoree_at, nl, 44, vl, 44 + nl + 2, 0, 0] {
                                    p32(&mut clr, x);
                                }
                                pw(&mut clr, &name);
                                pw(&mut clr, &ver);
                            }
                            let progid = c.progid.as_ref().map(|p| w16s(p));
                            let mask = c.misc.iter().enumerate().filter(|m| m.1.is_some()).fold(0u32, |a, (i, _)| a | 1 << i);
                            let mut v = Vec::new();
                            p32(&mut v, 120);
                            p32(&mut v, mask << 8);
                            p32(&mut v, c.model);
                            v.extend_from_slice(&c.clsid);
                            v.extend_from_slice(&alias);
                            v.extend_from_slice(&c.clsid);
                            v.extend_from_slice(&c.tlbid.unwrap_or_default());
                            p32(&mut v, module_len);
                            p32(&mut v, g + name_at);
                            p32(&mut v, progid.as_ref().map_or(0, |p| 2 * p.len() as u32));
                            p32(&mut v, if progid.is_some() { 120 + clr.len() as u32 } else { 0 });
                            p32(&mut v, clr.len() as u32);
                            p32(&mut v, if clr.is_empty() { 0 } else { 120 });
                            // miscstatus, content, thumbnail, icon, docprint (the struct's order)
                            for k in [0, 2, 3, 1, 4] {
                                p32(&mut v, c.misc[k].unwrap_or(0));
                            }
                            v.extend_from_slice(&clr);
                            if let Some(p) = &progid {
                                pw(&mut v, p);
                            }
                            v
                        }),
                    });
                }
            }
            if id == SEC_COM_PROGID_REDIRECTION {
                return Some(build_section(false, &ents, &aliases));
            }
        }
        SEC_COM_INTERFACE_REDIRECTION => {
            for (i, m) in asms.iter().enumerate() {
                for p in m.files.iter().flat_map(|f| &f.ps).chain(&m.ext_ps) {
                    let p = p.clone();
                    ents.push(Ent {
                        keys: vec![Key::Guid(p.iid)],
                        roster: i as u32 + 1,
                        data: Box::new(move |_, _| {
                            let name = w16s(&p.name);
                            let mut v = Vec::new();
                            p32(&mut v, 68);
                            p32(&mut v, p.nummethods.is_some() as u32 | (p.base.is_some() as u32) << 1);
                            v.extend_from_slice(&if p.external { p.ps32.unwrap_or(p.iid) } else { p.iid });
                            p32(&mut v, p.nummethods.unwrap_or(0));
                            v.extend_from_slice(&p.tlbid.unwrap_or_default());
                            v.extend_from_slice(&p.base.unwrap_or_default());
                            p32(&mut v, 2 * name.len() as u32);
                            p32(&mut v, 68);
                            pw(&mut v, &name);
                            v
                        }),
                    });
                }
            }
        }
        SEC_COM_TYPE_LIBRARY_REDIRECTION => {
            for (i, m) in asms.iter().enumerate() {
                for f in &m.files {
                    for t in &f.typelibs {
                        let t = t.clone();
                        let module = w16s(&f.name);
                        let name_at = names.add(&module);
                        let ml = 2 * module.len() as u32;
                        ents.push(Ent {
                            keys: vec![Key::Guid(t.tlbid)],
                            roster: i as u32 + 1,
                            data: Box::new(move |_, g| {
                                let help = w16s(&t.help);
                                let mut v = Vec::new();
                                for x in [32, 0, ml, g + name_at] {
                                    p32(&mut v, x);
                                }
                                p16(&mut v, t.lang);
                                p16(&mut v, t.flags);
                                p32(&mut v, 2 * help.len() as u32);
                                p32(&mut v, 32);
                                p16(&mut v, t.major);
                                p16(&mut v, t.minor);
                                pw(&mut v, &help);
                                v
                            }),
                        });
                    }
                }
            }
        }
        SEC_CLR_SURROGATES => {
            for (i, m) in asms.iter().enumerate() {
                for (clsid, name, ver) in &m.surrogates {
                    let (clsid, name, ver) = (*clsid, w16s(name), w16s(ver));
                    ents.push(Ent {
                        keys: vec![Key::Guid(clsid)],
                        roster: i as u32 + 1,
                        data: Box::new(move |_, _| {
                            let vl = 2 * ver.len() as u32;
                            let mut v = Vec::new();
                            p32(&mut v, 40);
                            p32(&mut v, 0);
                            v.extend_from_slice(&clsid);
                            for x in [40, vl, 40 + vl + 2, 2 * name.len() as u32] {
                                p32(&mut v, x);
                            }
                            pw(&mut v, &ver);
                            pw(&mut v, &name);
                            v
                        }),
                    });
                }
            }
        }
        SEC_WINRT_ACTIVATABLE_CLASSES => {
            for (i, m) in asms.iter().enumerate() {
                for f in &m.files {
                    let module = w16s(&f.name);
                    let at = names.add(&module);
                    let ml = 2 * module.len() as u32;
                    for (cls, model) in &f.winrt {
                        let model = *model;
                        ents.push(Ent {
                            keys: vec![Key::Str(w16s(cls))],
                            roster: i as u32 + 1,
                            data: Box::new(move |_, g| {
                                let mut v = Vec::new();
                                for x in [20, 0, ml, g + at, model] {
                                    p32(&mut v, x);
                                }
                                v
                            }),
                        });
                    }
                }
            }
        }
        _ => {}
    }
    Some(build_section(guid, &ents, &names.bytes))
}

// ---------------------------------------------------------------------------------------------
// built-in assemblies: what WinSxS holds on Windows

const CC_TOKEN: &str = "6595b64144ccf1df";
const VC_TOKEN: &str = "1fc8b3b9a1e18e3b";
/// Common Controls 6's window classes (its manifest's list)
const CC6_CLASSES: &[&str] = &[
    "Button", "ButtonListBox", "ComboBoxEx32", "ComboLBox", "Combobox", "Edit", "Listbox", "NativeFontCtl", "ReBarWindow32", "ScrollBar", "Static",
    "SysAnimate32", "SysDateTimePick32", "SysHeader32", "SysIPAddress32", "SysLink", "SysListView32", "SysMonthCal32", "SysPager", "SysTabControl32",
    "SysTreeView32", "ToolbarWindow32", "msctls_hotkey32", "msctls_progress32", "msctls_statusbar32", "msctls_trackbar32", "msctls_updown32",
    "tooltips_class32",
];

fn cur_arch() -> &'static str {
    if mem::x64() { "amd64" } else { "x86" }
}

/// The system assembly a reference names: (its manifest, its manifest's path)
fn builtin(r: &Ident) -> Option<(Manifest, String)> {
    let lname = r.name.to_ascii_lowercase();
    let token_ok = |t: &str| r.token.as_deref().is_none_or(|x| x.eq_ignore_ascii_case(t));
    let arch = match r.arch.as_deref() {
        None | Some("*") => cur_arch().to_string(),
        Some(a) => a.to_ascii_lowercase(),
    };
    let (token, version, files): (&str, [u16; 4], Vec<FileEnt>) = match lname.as_str() {
        "microsoft.windows.common-controls" if token_ok(CC_TOKEN) => {
            let major = r.version.map_or(6, |v| v[0]);
            if major != 6 && major != 5 {
                return None;
            }
            let v = if major == 6 { [6, 0, 19041, 1110] } else { [5, 82, 19041, 1110] };
            let classes = if major == 6 { CC6_CLASSES.iter().map(|c| (c.to_string(), true)).collect() } else { vec![] };
            (CC_TOKEN, v, vec![FileEnt { name: "comctl32.dll".into(), classes, ..Default::default() }])
        }
        "microsoft.windows.gdiplus" if token_ok(CC_TOKEN) => {
            let v = r.version.unwrap_or([1, 1, 19041, 1110]);
            (CC_TOKEN, v, vec![FileEnt { name: "gdiplus.dll".into(), ..Default::default() }])
        }
        "microsoft.vc80.crt" | "microsoft.vc90.crt" | "microsoft.vc80.mfc" | "microsoft.vc90.mfc" if token_ok(VC_TOKEN) => {
            let n = if lname.contains("80") { "80" } else { "90" };
            let names: &[&str] = if lname.ends_with("crt") { &["msvcr", "msvcp", "msvcm"] } else { &["mfc", "mfcm"] };
            let files = names.iter().map(|b| FileEnt { name: format!("{b}{n}.dll"), ..Default::default() }).collect();
            (VC_TOKEN, r.version.unwrap_or([9, 0, 30729, 9625]), files)
        }
        _ => return None,
    };
    let id = Ident { name: r.name.clone(), typ: Some("win32".into()), version: Some(version), arch: Some(arch), token: Some(token.into()), lang: None };
    let path = format!("C:\\Windows\\WinSxS\\manifests\\{}.manifest", id.dir_name());
    Some((Manifest { id, files, ..Default::default() }, path))
}

// ---------------------------------------------------------------------------------------------
// contexts

struct Asm {
    m: Manifest,
    /// the manifest's path ("" none)
    path: String,
    /// the directory its files are in (with a trailing backslash)
    base: String,
    /// the assembly's directory name (dependencies only)
    dir: Option<String>,
    flags: u32,
}

struct Actx {
    refs: u32,
    asms: Vec<Asm>,
    root_path: Option<String>,
    app_dir: Option<String>,
    /// sections made so far: id -> (guest address, length)
    sections: HashMap<u32, (u32, u32)>,
}

static CTXS: Mutex<Option<HashMap<u32, Actx>>> = Mutex::new(None);
/// The process default context (the exe's manifest): None not looked for yet, Some(0) none.
static DEFAULT: Mutex<Option<u32>> = Mutex::new(None);

fn with_ctxs<R>(f: impl FnOnce(&mut HashMap<u32, Actx>) -> R) -> R {
    let mut g = CTXS.lock().unwrap();
    f(g.get_or_insert_with(HashMap::default))
}

fn valid(h: u32) -> bool {
    h != 0 && h != INVALID_HANDLE_VALUE && with_ctxs(|m| m.contains_key(&h))
}

fn add_ref(h: u32) {
    with_ctxs(|m| {
        if let Some(a) = m.get_mut(&h) {
            a.refs += 1;
        }
    });
}

fn release(h: u32) {
    let gone = with_ctxs(|m| {
        let a = m.get_mut(&h)?;
        a.refs -= 1;
        if a.refs == 0 { m.remove(&h) } else { None }
    });
    if let Some(a) = gone {
        for (_, (p, _)) in a.sections {
            mem::heap_free(p);
        }
        mem::heap_free(h);
    }
}

fn register(a: Actx) -> u32 {
    let h = mem::heap_alloc(16, true);
    w32(h, 1);
    w32(h + 4, 0x7874_6341); // "Actx"
    with_ctxs(|m| m.insert(h, a));
    h
}

/// The directory part of a Windows path, with its backslash.
fn dir_of(p: &str) -> String {
    match p.rfind('\\') {
        Some(i) => p[..=i].to_string(),
        None => String::new(),
    }
}

fn read_file(w: &str) -> Result<Vec<u8>, u32> {
    match crate::files::resolve(w) {
        Some(h) if !crate::files::is_dir(&h) => kura::fs::read(&h).map_err(|_| ERROR_FILE_NOT_FOUND),
        _ => {
            let d = dir_of(&crate::files::full_path(w));
            Err(if d.len() <= 3 || crate::files::resolve(&d).is_some() { ERROR_FILE_NOT_FOUND } else { ERROR_PATH_NOT_FOUND })
        }
    }
}

/// The languages a manifest resource is taken in, best first.
fn lang_order() -> Vec<u16> {
    let ui = crate::k32::ui_lang() as u16;
    let mut v = Vec::new();
    for l in [0, ui, ui & 0x3ff, 0x409, 1] {
        if !v.contains(&l) {
            v.push(l);
        }
    }
    v
}

/// The manifest a module's RT_MANIFEST resources give a dependency: the one with the smallest
/// number, in the best language. None: it has none.
fn dll_manifest(tree: &crate::resupdate::Tree) -> Option<Vec<u8>> {
    use crate::resupdate::Id;
    let names = tree.get(&Id::Int(24))?;
    let (_, langs) = names.iter().filter(|(k, _)| matches!(k, Id::Int(_))).min_by_key(|(k, _)| match k {
        Id::Int(n) => *n,
        _ => u16::MAX,
    })?;
    lang_order().iter().find_map(|l| langs.get(l)).or_else(|| langs.values().next()).map(|x| x.1.clone())
}

/// Looks for the assembly `r` beside the application, in `dir`. Ok(None): not there.
fn probe(r: &Ident, dir: &str) -> Result<Option<(Manifest, String)>, u32> {
    let mut d = dir.to_string();
    if !d.is_empty() && !d.ends_with('\\') {
        d.push('\\');
    }
    if let Some(l) = r.lang.as_deref().filter(|l| *l != "*" && !l.is_empty() && !l.eq_ignore_ascii_case("neutral")) {
        d = format!("{d}{l}\\");
    }
    let n = &r.name;
    for cand in [format!("{d}{n}.dll"), format!("{d}{n}.manifest"), format!("{d}{n}\\{n}.dll"), format!("{d}{n}\\{n}.manifest")] {
        let Ok(bytes) = read_file(&cand) else { continue };
        let text = if cand.ends_with(".dll") {
            match crate::resupdate::read(&bytes).as_ref().and_then(dll_manifest) {
                Some(t) => t,
                None => continue,
            }
        } else {
            bytes
        };
        let m = parse_manifest(&text).ok_or(ERROR_SXS_CANT_GEN_ACTCTX)?;
        if !m.id.satisfies(r) {
            return Err(ERROR_SXS_CANT_GEN_ACTCTX);
        }
        return Ok(Some((m, crate::files::full_path(&cand))));
    }
    Ok(None)
}

/// The assemblies of a context: `root` and what it depends on, each once.
fn resolve_deps(root: Manifest, root_path: &str, dir: &str) -> Result<Vec<Asm>, u32> {
    let mut asms = vec![Asm { m: root, path: root_path.to_string(), base: dir_of(root_path), dir: None, flags: 1 }];
    let mut i = 0;
    while i < asms.len() && asms.len() < 64 {
        let deps = asms[i].m.deps.clone();
        for d in deps {
            if asms.iter().skip(1).any(|a| a.m.id.satisfies(&d.id)) {
                continue;
            }
            let found = match builtin(&d.id) {
                Some((m, p)) => Some((m, p, "C:\\Windows\\WinSxS\\".to_string())),
                None => probe(&d.id, dir)?.map(|(m, p)| {
                    let b = dir_of(&p);
                    (m, p, b)
                }),
            };
            match found {
                Some((m, path, base)) => {
                    let dirn = m.id.dir_name();
                    let base = if base.starts_with("C:\\Windows\\WinSxS") { format!("{base}{dirn}\\") } else { base };
                    asms.push(Asm { m, path, base, dir: Some(dirn), flags: 0x10 });
                }
                None if d.optional => {}
                None => return Err(ERROR_SXS_CANT_GEN_ACTCTX),
            }
        }
        i += 1;
    }
    Ok(asms)
}

/// What CreateActCtx was asked for
#[derive(Default)]
struct Req {
    flags: u32,
    source: Option<String>,
    asm_dir: Option<String>,
    res: Option<crate::pe::ResId>,
    lang: u16,
    app_name: Option<String>,
    module: u32,
}

const ACTCTX_FLAG_LANGID_VALID: u32 = 2;
const ACTCTX_FLAG_ASSEMBLY_DIRECTORY_VALID: u32 = 4;
const ACTCTX_FLAG_RESOURCE_NAME_VALID: u32 = 8;
const ACTCTX_FLAG_SET_PROCESS_DEFAULT: u32 = 0x10;
const ACTCTX_FLAG_APPLICATION_NAME_VALID: u32 = 0x20;
const ACTCTX_FLAG_HMODULE_VALID: u32 = 0x80;

fn exe_dir() -> String {
    dir_of(&crate::k32::exe_path(false))
}

/// A resource of a PE file's tree (RT_MANIFEST `name`, language `lang` or the best one)
fn tree_resource(tree: &crate::resupdate::Tree, name: &crate::pe::ResId, lang: Option<u16>) -> Result<Vec<u8>, u32> {
    use crate::resupdate::Id;
    let names = tree.get(&Id::Int(24)).ok_or(ERROR_RESOURCE_TYPE_NOT_FOUND)?;
    let key = match name {
        crate::pe::ResId::Int(n) => Id::Int(*n as u16),
        crate::pe::ResId::Name(s) => Id::Name(s.clone()),
    };
    let langs = names
        .iter()
        .find(|(k, _)| match (k, &key) {
            (Id::Name(a), Id::Name(b)) => eq_nocase(a, b),
            (a, b) => a == &b,
        })
        .map(|x| x.1)
        .ok_or(1814u32)?;
    match lang {
        Some(l) if l != 0 => langs.get(&l).map(|x| x.1.clone()).ok_or(1815),
        _ => lang_order().iter().find_map(|l| langs.get(l)).or_else(|| langs.values().next()).map(|x| x.1.clone()).ok_or(1815),
    }
}

fn create(req: &Req) -> Result<u32, u32> {
    if req.flags & ACTCTX_FLAG_RESOURCE_NAME_VALID != 0 && req.res.is_none() {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let lang = (req.flags & ACTCTX_FLAG_LANGID_VALID != 0).then_some(req.lang);
    let (bytes, root_path, dir) = if req.flags & ACTCTX_FLAG_HMODULE_VALID != 0 {
        let Some(name) = req.res.as_ref().filter(|_| req.flags & ACTCTX_FLAG_RESOURCE_NAME_VALID != 0) else {
            return Err(ERROR_RESOURCE_TYPE_NOT_FOUND);
        };
        // (a module loaded as a data file has no context of its own)
        if req.module & 3 != 0 {
            return Err(1812);
        }
        let path = crate::pe::with(req.module, |m| m.path.clone()).ok_or(ERROR_INVALID_PARAMETER)?;
        let e = crate::pe::find_resource_lang(req.module, &crate::pe::ResId::Int(24), name, lang.map(|l| l as u32))?;
        let (p, n) = crate::pe::resource_data(req.module, e);
        (mem::slice(p, n).to_vec(), path.clone(), req.asm_dir.clone().unwrap_or_else(|| dir_of(&path)))
    } else {
        let src = req.source.as_deref().ok_or(ERROR_INVALID_PARAMETER)?;
        let full = match &req.asm_dir {
            Some(d) if !src.contains(':') && !src.starts_with('\\') => crate::files::full_path(&format!("{}\\{src}", d.trim_end_matches('\\'))),
            _ => crate::files::full_path(src),
        };
        let data = read_file(&full)?;
        let data = if data.starts_with(b"MZ") {
            let Some(name) = req.res.as_ref().filter(|_| req.flags & ACTCTX_FLAG_RESOURCE_NAME_VALID != 0) else {
                return Err(ERROR_RESOURCE_TYPE_NOT_FOUND);
            };
            let tree = crate::resupdate::read(&data).ok_or(ERROR_SXS_CANT_GEN_ACTCTX)?;
            tree_resource(&tree, name, lang)?
        } else {
            data
        };
        let dir = req.asm_dir.clone().unwrap_or_else(|| dir_of(&full));
        (data, full, dir)
    };
    let m = parse_manifest(&bytes).ok_or(ERROR_SXS_CANT_GEN_ACTCTX)?;
    let asms = resolve_deps(m, &root_path, &dir)?;
    let app_dir = if req.flags & ACTCTX_FLAG_APPLICATION_NAME_VALID != 0 { req.app_name.clone() } else { Some(exe_dir()) };
    let h = register(Actx { refs: 1, asms, root_path: Some(root_path), app_dir, sections: HashMap::default() });
    if req.flags & ACTCTX_FLAG_SET_PROCESS_DEFAULT != 0 {
        let mut d = DEFAULT.lock().unwrap();
        if d.is_none_or(|h| h == 0) {
            add_ref(h);
            *d = Some(h);
        }
    }
    Ok(h)
}

/// The process default context: the exe's RT_MANIFEST 1, else a <exe>.manifest beside it.
/// 0: none.
fn process_default() -> u32 {
    if let Some(h) = *DEFAULT.lock().unwrap() {
        return h;
    }
    let exe = crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.base).unwrap_or(0);
    let mut h = 0;
    if exe != 0 {
        let r = Req { flags: ACTCTX_FLAG_HMODULE_VALID | ACTCTX_FLAG_RESOURCE_NAME_VALID, res: Some(crate::pe::ResId::Int(1)), module: exe, ..Default::default() };
        h = create(&r).unwrap_or(0);
        if h == 0 {
            let path = format!("{}.manifest", crate::k32::exe_path(false));
            if crate::files::resolve(&path).is_some() {
                h = create(&Req { source: Some(path), ..Default::default() }).unwrap_or(0);
            }
        }
    }
    let mut d = DEFAULT.lock().unwrap();
    *d.get_or_insert(h)
}

// ---------------------------------------------------------------------------------------------
// the thread's activation stack (TEB ActivationContextStackPointer)

const FRAME_HEAP: u32 = 8;

fn teb_stack_slot() -> u32 {
    crate::cur().teb + if mem::x64() { 0x2c8 } else { 0x1a8 }
}

/// The thread's ACTIVATION_CONTEXT_STACK (made on first use)
fn stack() -> u32 {
    let slot = teb_stack_slot();
    let s = rp(slot);
    if s != 0 {
        return s;
    }
    let ps = mem::ps();
    let s = mem::heap_alloc(3 * ps + 12, true);
    // FrameListCache: an empty list
    mem::wp(s + ps, s + ps);
    mem::wp(s + 2 * ps, s + ps);
    mem::wp(slot, s);
    s
}

/// The active frame (0 none)
fn active_frame() -> u32 {
    if !crate::on_guest_thread() {
        return 0;
    }
    let s = rp(teb_stack_slot());
    if s == 0 { 0 } else { rp(s) }
}

/// The active context (0: none, the process default answers)
fn active() -> u32 {
    match active_frame() {
        0 => 0,
        f => rp(f + mem::ps()),
    }
}

fn push_frame(frame: u32, h: u32, flags: u32) {
    let s = stack();
    let ps = mem::ps();
    mem::wp(frame, rp(s));
    mem::wp(frame + ps, h);
    w32(frame + 2 * ps, flags);
    mem::wp(s, frame);
    if valid(h) {
        add_ref(h);
    }
}

/// Contexts a new thread starts with, by its thread object: its creator's active one
/// (CreateThread activates it on the new thread as Windows' kernel32 does).
static INHERIT: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());

/// CreateThread: the running thread's active context goes to the thread `obj` will start.
pub fn inherit(obj: u32) {
    let h = active();
    if valid(h) {
        add_ref(h);
        INHERIT.lock().unwrap().push((obj, h));
    }
}

/// A thread `obj` that could not be made: the reference `inherit` took goes.
pub fn forget(obj: u32) {
    let h = {
        let mut v = INHERIT.lock().unwrap();
        let Some(i) = v.iter().position(|x| x.0 == obj) else { return };
        v.remove(i).1
    };
    release(h);
}

/// On the new thread of `obj`: the context its creator had active, activated.
pub fn adopt(obj: u32) {
    let h = {
        let mut v = INHERIT.lock().unwrap();
        let Some(i) = v.iter().position(|x| x.0 == obj) else { return };
        v.remove(i).1
    };
    activate(h);
    release(h);
}

/// RtlActivateActivationContext: a heap frame; the cookie is its address.
fn activate(h: u32) -> u32 {
    let ps = mem::ps();
    let f = mem::heap_alloc(2 * ps + 4 + 4, true);
    push_frame(f, h, FRAME_HEAP);
    f
}

/// Pops frames down to and including `frame`. False: it is not on the stack.
fn pop_to(frame: u32) -> bool {
    let s = rp(teb_stack_slot());
    if s == 0 || frame == 0 {
        return false;
    }
    let ps = mem::ps();
    let mut f = rp(s);
    let mut seen = HashSet::default();
    while f != 0 && f != frame && seen.insert(f) {
        f = rp(f);
    }
    if f != frame {
        return false;
    }
    loop {
        let top = rp(s);
        let prev = rp(top);
        let h = rp(top + ps);
        let flags = r32(top + 2 * ps);
        mem::wp(s, prev);
        if valid(h) {
            release(h);
        }
        if flags & FRAME_HEAP != 0 {
            mem::heap_free(top);
        }
        if top == frame {
            return true;
        }
    }
}

fn free_stack() {
    let s = rp(teb_stack_slot());
    if s == 0 {
        return;
    }
    while rp(s) != 0 {
        let f = rp(s);
        pop_to(f);
    }
}

// ---------------------------------------------------------------------------------------------
// queries

/// The section `id` of context `h` in guest memory: (address, length)
fn guest_section(h: u32, id: u32) -> Option<(u32, u32)> {
    let cached = with_ctxs(|m| m.get(&h).and_then(|a| a.sections.get(&id).copied()));
    if let Some(s) = cached {
        return Some(s);
    }
    let bytes = with_ctxs(|m| {
        let a = m.get(&h)?;
        let ms: Vec<Manifest> = a.asms.iter().map(|x| x.m.clone()).collect();
        make_section(id, &ms)
    })?;
    let p = mem::heap_alloc(bytes.len() as u32, false);
    if p == 0 {
        return None;
    }
    mem::write(p, &bytes);
    let s = (p, bytes.len() as u32);
    with_ctxs(|m| {
        if let Some(a) = m.get_mut(&h) {
            a.sections.insert(id, s);
        }
    });
    Some(s)
}

/// ACTCTX_SECTION_KEYED_DATA field offsets: (lpData, ulLength, lpSectionGlobalData,
/// ulSectionGlobalDataLength, lpSectionBase, ulSectionTotalLength, hActCtx,
/// ulAssemblyRosterIndex, ulFlags)
fn keyed_offsets() -> [u32; 9] {
    if mem::x64() { [8, 16, 24, 32, 40, 48, 56, 64, 68] } else { [8, 12, 16, 20, 24, 28, 32, 36, 40] }
}

const FIND_ACTCTX_SECTION_KEY_RETURN_HACTCTX: u32 = 1;
const FIND_ACTCTX_SECTION_KEY_RETURN_FLAGS: u32 = 2;

/// RtlFindActivationContextSection{String,Guid}: the active context, then the process
/// default. `data` 0: only whether it is there.
fn find(flags: u32, id: u32, key: &Key, data: u32) -> u32 {
    let Some(guid) = is_guid_section(id) else { return STATUS_SXS_SECTION_NOT_FOUND };
    if guid != matches!(key, Key::Guid(_)) {
        return STATUS_SXS_SECTION_NOT_FOUND;
    }
    let o = keyed_offsets();
    if data != 0 && r32(data) < o[7] {
        return STATUS_INVALID_PARAMETER;
    }
    let act = active();
    let def = process_default();
    let mut searched = false;
    for (h, is_default) in [(act, false), (def, true)] {
        if h == 0 || !valid(h) || (is_default && h == act) {
            continue;
        }
        searched = true;
        let Some((base, len)) = guest_section(h, id) else { continue };
        let sec = mem::slice(base, len);
        let Some((doff, dlen, roster)) = section_find(sec, key) else { continue };
        if data != 0 {
            let cb = r32(data);
            let (goff, glen) = section_global(sec);
            w32(data + 4, 1);
            mem::wp(data + o[0], base + doff);
            w32(data + o[1], dlen);
            mem::wp(data + o[2], if glen != 0 { base + goff } else { 0 });
            w32(data + o[3], glen);
            mem::wp(data + o[4], base);
            w32(data + o[5], len);
            if flags & FIND_ACTCTX_SECTION_KEY_RETURN_HACTCTX != 0 {
                add_ref(h);
                mem::wp(data + o[6], h);
            } else {
                mem::wp(data + o[6], 0);
            }
            if cb >= o[7] + 4 {
                w32(data + o[7], roster);
            }
            if flags & FIND_ACTCTX_SECTION_KEY_RETURN_FLAGS != 0 && cb >= o[8] + 4 {
                w32(data + o[8], is_default as u32);
            }
        }
        return STATUS_SUCCESS;
    }
    let _ = searched;
    STATUS_SXS_KEY_NOT_FOUND
}

const QUERY_ACTCTX_FLAG_USE_ACTIVE_ACTCTX: u32 = 4;
const QUERY_ACTCTX_FLAG_ACTCTX_IS_HMODULE: u32 = 8;
const QUERY_ACTCTX_FLAG_ACTCTX_IS_ADDRESS: u32 = 0x10;
const QUERY_ACTCTX_FLAG_NO_ADDREF: u32 = 0x8000_0000;
const PATH_NONE: u32 = 1;
const PATH_WIN32_FILE: u32 = 2;

/// What a query needs to know of a context (an empty one: no context at all)
struct Info {
    asms: Vec<(Manifest, String, Option<String>, u32)>,
    root_path: Option<String>,
    app_dir: Option<String>,
}

fn info_of(h: u32) -> Info {
    with_ctxs(|m| match m.get(&h) {
        Some(a) => Info {
            asms: a.asms.iter().map(|x| (x.m.clone(), x.path.clone(), x.dir.clone(), x.flags)).collect(),
            root_path: a.root_path.clone(),
            app_dir: a.app_dir.clone(),
        },
        None => Info { asms: vec![], root_path: None, app_dir: None },
    })
}

/// Writes `s` NUL-ended at `*at` and moves `at` past it; returns where it went.
fn put_w(at: &mut u32, s: &str) -> u32 {
    let w = w16s(s);
    let p = *at;
    for (i, &c) in w.iter().enumerate() {
        w16(p + 2 * i as u32, c);
    }
    w16(p + 2 * w.len() as u32, 0);
    *at += 2 * w.len() as u32 + 2;
    p
}

fn wlen(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

fn query(flags: u32, mut h: u32, sub: u32, class: u32, buf: u32, size: u32, retlen: u32) -> u32 {
    if retlen != 0 {
        mem::wp(retlen, 0);
    }
    let set_ret = |n: u32| {
        if retlen != 0 {
            mem::wp(retlen, n);
        }
    };
    if flags & QUERY_ACTCTX_FLAG_USE_ACTIVE_ACTCTX != 0 {
        if h != 0 {
            return STATUS_INVALID_PARAMETER;
        }
        h = active();
    } else if flags & (QUERY_ACTCTX_FLAG_ACTCTX_IS_HMODULE | QUERY_ACTCTX_FLAG_ACTCTX_IS_ADDRESS) != 0 {
        h = 0;
    }
    if h != 0 && !valid(h) {
        return STATUS_INVALID_PARAMETER;
    }
    let ps = mem::ps();
    let ctx = if h != 0 { h } else { process_default() };
    match class {
        // ActivationContextBasicInformation
        1 => {
            let need = 2 * ps;
            if size < need || buf == 0 {
                set_ret(need);
                return STATUS_BUFFER_TOO_SMALL;
            }
            mem::wp(buf, h);
            w32(buf + ps, 0);
            if flags & QUERY_ACTCTX_FLAG_NO_ADDREF == 0 && valid(h) {
                add_ref(h);
            }
            set_ret(need);
        }
        // ActivationContextDetailedInformation
        2 => {
            let i = info_of(ctx);
            let hdr = if mem::x64() { 64 } else { 48 };
            let root = i.root_path.clone().filter(|_| !i.asms.is_empty());
            let app = i.app_dir.clone().filter(|_| !i.asms.is_empty());
            let need = hdr + root.as_ref().map_or(0, |s| 2 * wlen(s) + 2) + app.as_ref().map_or(0, |s| 2 * wlen(s) + 2);
            if size < need || buf == 0 {
                set_ret(need);
                return STATUS_BUFFER_TOO_SMALL;
            }
            let empty = i.asms.is_empty();
            let vals = [
                0,
                (!empty) as u32,
                i.asms.len() as u32,
                if empty { 0 } else { PATH_WIN32_FILE },
                root.as_ref().map_or(0, |s| wlen(s)),
                if empty { 0 } else { PATH_NONE },
                0,
                if empty { 0 } else { PATH_WIN32_FILE },
                app.as_ref().map_or(0, |s| wlen(s)),
            ];
            for (k, v) in vals.iter().enumerate() {
                w32(buf + 4 * k as u32, *v);
            }
            if mem::x64() {
                w32(buf + 36, 0);
            }
            let p0 = if mem::x64() { 40 } else { 36 };
            let mut at = buf + hdr;
            let rp_ = root.as_ref().map_or(0, |s| put_w(&mut at, s));
            let ap = app.as_ref().map_or(0, |s| put_w(&mut at, s));
            mem::wp(buf + p0, rp_);
            mem::wp(buf + p0 + ps, 0);
            mem::wp(buf + p0 + 2 * ps, ap);
            set_ret(need);
        }
        // AssemblyDetailedInformationInActivationContext
        3 => {
            let i = info_of(ctx);
            if sub == 0 {
                return STATUS_INVALID_PARAMETER;
            }
            let idx = r32(sub);
            if idx == 0 || idx as usize > i.asms.len() {
                return STATUS_INVALID_PARAMETER;
            }
            let (m, path, dir, aflags) = &i.asms[idx as usize - 1];
            let enc = m.id.encoded();
            let hdr = if mem::x64() { 104 } else { 88 };
            let need = hdr + 2 * wlen(&enc) + 2 + if path.is_empty() { 0 } else { 2 * wlen(path) + 2 } + dir.as_ref().map_or(0, |d| 2 * wlen(d) + 2);
            if size < need || buf == 0 {
                set_ret(need);
                return STATUS_BUFFER_TOO_SMALL;
            }
            mem::fill(buf, hdr, 0);
            w32(buf, *aflags);
            w32(buf + 4, 2 * wlen(&enc));
            w32(buf + 8, PATH_WIN32_FILE);
            w32(buf + 12, 2 * wlen(path));
            w32(buf + 24, PATH_NONE);
            w32(buf + 44, 1);
            w32(buf + 60, dir.as_ref().map_or(0, |d| 2 * wlen(d)));
            let mut at = buf + hdr;
            let pe = put_w(&mut at, &enc);
            let pp = if path.is_empty() { 0 } else { put_w(&mut at, path) };
            let pd = dir.as_ref().map_or(0, |d| put_w(&mut at, d));
            mem::wp(buf + 64, pe);
            mem::wp(buf + 64 + ps, pp);
            mem::wp(buf + 64 + 2 * ps, 0);
            mem::wp(buf + 64 + 3 * ps, pd);
            w32(buf + 64 + 4 * ps, m.files.len() as u32);
            set_ret(need);
        }
        // FileInformationInAssemblyOfAssemblyInActivationContext
        4 => {
            let i = info_of(ctx);
            if sub == 0 {
                return STATUS_INVALID_PARAMETER;
            }
            let (ai, fi) = (r32(sub) as usize, r32(sub + 4) as usize);
            let Some(f) = i.asms.get(ai).and_then(|a| a.0.files.get(fi)) else { return STATUS_INVALID_PARAMETER };
            let hdr = if mem::x64() { 32 } else { 20 };
            let need = hdr + 2 * wlen(&f.name) + 2;
            if size < need || buf == 0 {
                set_ret(need);
                return STATUS_BUFFER_TOO_SMALL;
            }
            w32(buf, 2);
            w32(buf + 4, 2 * wlen(&f.name));
            w32(buf + 8, 0);
            let p0 = if mem::x64() { 16 } else { 12 };
            let mut at = buf + hdr;
            let pn = put_w(&mut at, &f.name);
            mem::wp(buf + p0, pn);
            mem::wp(buf + p0 + ps, 0);
        }
        // RunlevelInformationInActivationContext
        5 => {
            let i = info_of(ctx);
            if size < 12 || buf == 0 {
                set_ret(12);
                return STATUS_BUFFER_TOO_SMALL;
            }
            let (lvl, ui) = i.asms.first().map_or((0, false), |a| (a.0.run_level, a.0.ui_access));
            w32(buf, 0);
            w32(buf + 4, lvl);
            w32(buf + 8, ui as u32);
            set_ret(12);
        }
        // CompatibilityInformationInActivationContext
        6 => {
            let i = info_of(ctx);
            let el: Vec<Compat> = i.asms.first().map_or(vec![], |a| a.0.compat.clone());
            let need = 8 + 32 * el.len() as u32;
            if size < need || buf == 0 {
                set_ret(need);
                return STATUS_BUFFER_TOO_SMALL;
            }
            mem::fill(buf, need, 0);
            w32(buf, el.len() as u32);
            for (k, e) in el.iter().enumerate() {
                let p = buf + 8 + 32 * k as u32;
                match e {
                    Compat::Os(g) => {
                        mem::write(p, g);
                        w32(p + 16, 1);
                    }
                    Compat::MaxVersion(v) => {
                        w32(p + 16, 3);
                        mem::w64(p + 24, *v);
                    }
                }
            }
            set_ret(need);
        }
        _ => return STATUS_INVALID_PARAMETER,
    }
    STATUS_SUCCESS
}

/// RtlQueryActivationContextApplicationSettings
fn settings(flags: u32, h: u32, ns: u32, name: u32, buf: u32, size: u32, written: u32) -> u32 {
    if flags != 0 || name == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    let ns = if ns == 0 { SETTINGS_NS[0].to_string() } else { String::from_utf16_lossy(&wstr(ns)) };
    if !SETTINGS_NS.contains(&ns.as_str()) {
        return STATUS_INVALID_PARAMETER;
    }
    let name = String::from_utf16_lossy(&wstr(name));
    let ctx = if h != 0 { h } else { process_default() };
    let i = info_of(ctx);
    let Some(v) = i.asms.first().and_then(|a| a.0.settings.iter().find(|s| s.0 == ns && s.1 == name)).map(|s| s.2.clone()) else {
        return STATUS_SXS_KEY_NOT_FOUND;
    };
    let w = w16s(&v);
    if written != 0 {
        mem::wp(written, w.len() as u32 + 1);
    }
    if (size as usize) < w.len() {
        return STATUS_BUFFER_TOO_SMALL;
    }
    for (k, &c) in w.iter().enumerate() {
        w16(buf + 2 * k as u32, c);
    }
    w16(buf + 2 * w.len() as u32, 0);
    STATUS_SUCCESS
}

// ---------------------------------------------------------------------------------------------
// entry points

/// ACTCTX{A,W} at `p`
fn read_actctx(p: u32, wide: bool) -> Result<Req, u32> {
    if p == 0 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let ps = mem::ps();
    let x = mem::x64();
    // lpSource, wProcessorArchitecture, wLangId, lpAssemblyDirectory, lpResourceName,
    // lpApplicationName, hModule
    let [src, _arch, lang, dir, res, app, hmod] = if x { [8, 16, 18, 24, 32, 40, 48] } else { [8, 12, 14, 16, 20, 24, 28] };
    let cb = r32(p);
    let flags = r32(p + 4);
    let s = |a: u32| -> Option<String> {
        let v = rp(a);
        (v != 0).then(|| if wide { String::from_utf16_lossy(&wstr(v)) } else { crate::k32::arg_str(v, false) })
    };
    if cb < src + ps {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let need = |f: u32, off: u32, n: u32| flags & f == 0 || cb >= off + n;
    if !need(ACTCTX_FLAG_LANGID_VALID, lang, 2)
        || !need(ACTCTX_FLAG_ASSEMBLY_DIRECTORY_VALID, dir, ps)
        || !need(ACTCTX_FLAG_RESOURCE_NAME_VALID, res, ps)
        || !need(ACTCTX_FLAG_APPLICATION_NAME_VALID, app, ps)
        || !need(ACTCTX_FLAG_HMODULE_VALID, hmod, ps)
    {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let mut r = Req { flags, source: s(p + src), ..Default::default() };
    if flags & ACTCTX_FLAG_LANGID_VALID != 0 {
        r.lang = r16(p + lang);
    }
    if flags & ACTCTX_FLAG_ASSEMBLY_DIRECTORY_VALID != 0 {
        r.asm_dir = s(p + dir);
    }
    if flags & ACTCTX_FLAG_RESOURCE_NAME_VALID != 0 {
        let v = rp(p + res);
        r.res = (v != 0).then(|| crate::pe::ResId::from_arg(v, wide));
    }
    if flags & ACTCTX_FLAG_APPLICATION_NAME_VALID != 0 {
        r.app_name = s(p + app);
    }
    if flags & ACTCTX_FLAG_HMODULE_VALID != 0 {
        r.module = rp(p + hmod);
    }
    Ok(r)
}

fn create_api(c: &Ctx, wide: bool) -> u32 {
    match read_actctx(c.a(0), wide).and_then(|r| create(&r)) {
        Ok(h) => h,
        Err(e) => {
            last_error(e);
            INVALID_HANDLE_VALUE
        }
    }
}
fn CreateActCtxW(c: &mut Ctx) -> u32 {
    create_api(c, true)
}
fn CreateActCtxA(c: &mut Ctx) -> u32 {
    create_api(c, false)
}
/// RtlCreateActivationContext(handle out, ACTCTXW)
fn RtlCreateActivationContext(c: &mut Ctx) -> u32 {
    match read_actctx(c.a(1), true).and_then(|r| create(&r)) {
        Ok(h) => {
            mem::putp(c.a(0), h);
            STATUS_SUCCESS
        }
        Err(ERROR_INVALID_PARAMETER) => STATUS_INVALID_PARAMETER,
        Err(_) => STATUS_SXS_CANT_GEN_ACTCTX,
    }
}

fn ActivateActCtx(c: &mut Ctx) -> u32 {
    let f = activate(c.a(0));
    if c.a(1) != 0 {
        mem::putp(c.a(1), f);
    }
    1
}
/// RtlActivateActivationContext(flags, context, cookie)
fn RtlActivateActivationContext(c: &mut Ctx) -> u32 {
    let f = activate(c.a(1));
    if c.a(2) != 0 {
        mem::putp(c.a(2), f);
    }
    STATUS_SUCCESS
}
/// RtlActivateActivationContextEx(flags, teb, context, cookie)
fn RtlActivateActivationContextEx(c: &mut Ctx) -> u32 {
    let f = activate(c.a(2));
    if c.a(3) != 0 {
        mem::putp(c.a(3), f);
    }
    STATUS_SUCCESS
}
fn DeactivateActCtx(c: &mut Ctx) -> u32 {
    if pop_to(c.a(1)) {
        1
    } else {
        last_error(ERROR_SXS_INVALID_DEACTIVATION);
        0
    }
}
fn RtlDeactivateActivationContext(c: &mut Ctx) -> u32 {
    pop_to(c.a(1));
    0
}
/// The caller's frame inside RTL_CALLER_ALLOCATED_ACTIVATION_CONTEXT_STACK_FRAME_EXTENDED
fn unsafe_frame(ext: u32) -> u32 {
    ext + if mem::x64() { 16 } else { 8 }
}
/// (fastcall) RtlActivateActivationContextUnsafeFast(frame, context)
fn RtlActivateActivationContextUnsafeFast(c: &mut Ctx) -> u32 {
    let (ext, h) = (c.cpu.gpr(1) as u32, c.cpu.gpr(2) as u32);
    let f = unsafe_frame(ext);
    push_frame(f, h, 0);
    f
}
/// (fastcall) RtlDeactivateActivationContextUnsafeFast(frame)
fn RtlDeactivateActivationContextUnsafeFast(c: &mut Ctx) -> u32 {
    let f = unsafe_frame(c.cpu.gpr(1) as u32);
    pop_to(f);
    f
}
fn RtlFreeThreadActivationContextStack(_: &mut Ctx) -> u32 {
    free_stack();
    0
}
fn GetCurrentActCtx(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let h = active();
    if valid(h) {
        add_ref(h);
    }
    mem::putp(c.a(0), h);
    1
}
fn RtlGetActiveActivationContext(c: &mut Ctx) -> u32 {
    let h = active();
    if valid(h) {
        add_ref(h);
    }
    mem::putp(c.a(0), h);
    STATUS_SUCCESS
}
fn RtlIsActivationContextActive(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let ps = mem::ps();
    let mut f = active_frame();
    let mut n = 0;
    while f != 0 && n < 4096 {
        if rp(f + ps) == h {
            return 1;
        }
        f = rp(f);
        n += 1;
    }
    0
}
fn ReleaseActCtx(c: &mut Ctx) -> u32 {
    if valid(c.a(0)) {
        release(c.a(0));
    }
    0
}
fn AddRefActCtx(c: &mut Ctx) -> u32 {
    if valid(c.a(0)) {
        add_ref(c.a(0));
    }
    0
}
fn ZombifyActCtx(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 || c.a(0) == INVALID_HANDLE_VALUE {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    1
}
fn RtlZombifyActivationContext(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 { STATUS_INVALID_PARAMETER } else { STATUS_SUCCESS }
}

fn ret_bool(st: u32) -> u32 {
    if st == STATUS_SUCCESS {
        1
    } else {
        last_error(win32(st));
        0
    }
}

/// QueryActCtxW(flags, context, subinstance, class, buffer, size, returned)
fn QueryActCtxW(c: &mut Ctx) -> u32 {
    ret_bool(query(c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6)))
}
fn RtlQueryInformationActivationContext(c: &mut Ctx) -> u32 {
    query(c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6))
}
/// RtlQueryInformationActiveActivationContext(class, buffer, size, returned)
fn RtlQueryInformationActiveActivationContext(c: &mut Ctx) -> u32 {
    query(QUERY_ACTCTX_FLAG_USE_ACTIVE_ACTCTX, 0, 0, c.a(0), c.a(1), c.a(2), c.a(3))
}
/// QueryActCtxSettingsW(flags, context, namespace, name, buffer, size, written)
fn QueryActCtxSettingsW(c: &mut Ctx) -> u32 {
    ret_bool(settings(c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6)))
}
fn RtlQueryActivationContextApplicationSettings(c: &mut Ctx) -> u32 {
    settings(c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6))
}

fn find_api(flags: u32, id: u32, key: Option<Key>, data: u32) -> u32 {
    let o = keyed_offsets();
    if data == 0 || flags & !7 != 0 || r32(data) < o[7] {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let Some(key) = key else {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    };
    ret_bool(find(flags, id, &key, data))
}
/// FindActCtxSectionStringW(flags, extension guid, section, string, data)
fn FindActCtxSectionStringW(c: &mut Ctx) -> u32 {
    let s = c.a(3);
    find_api(c.a(0), c.a(2), (s != 0).then(|| Key::Str(wstr(s))), c.a(4))
}
fn FindActCtxSectionStringA(c: &mut Ctx) -> u32 {
    let s = c.a(3);
    find_api(c.a(0), c.a(2), (s != 0).then(|| Key::Str(crate::text::a2w(&cstr(s)))), c.a(4))
}
fn guid_at(p: u32) -> Guid {
    mem::slice(p, 16).try_into().unwrap()
}
/// FindActCtxSectionGuid(flags, extension guid, section, guid, data)
fn FindActCtxSectionGuid(c: &mut Ctx) -> u32 {
    let g = c.a(3);
    find_api(c.a(0), c.a(2), (g != 0).then(|| Key::Guid(guid_at(g))), c.a(4))
}
/// RtlFindActivationContextSectionString(flags, extension guid, section, UNICODE_STRING, data)
fn RtlFindActivationContextSectionString(c: &mut Ctx) -> u32 {
    let us = c.a(3);
    if us == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    let s = mem::wstrn(rp(us + mem::ps()), r16(us) as u32 / 2);
    find(c.a(0), c.a(2), &Key::Str(s), c.a(4))
}
fn RtlFindActivationContextSectionGuid(c: &mut Ctx) -> u32 {
    if c.a(3) == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    find(c.a(0), c.a(2), &Key::Guid(guid_at(c.a(3))), c.a(4))
}

/// The DLL a LoadLibrary of `name` (a bare file name) loads in the active context: the
/// redirected file of the assembly that lists it, when it is there.
pub fn redirect_dll(name: &str) -> Option<String> {
    if name.contains('\\') || name.contains('/') {
        return None;
    }
    let base = name.rsplit_once('.').map_or(format!("{name}.dll"), |_| name.to_string());
    let key = Key::Str(w16s(&base));
    for h in [active(), process_default()] {
        if !valid(h) {
            continue;
        }
        let hit = with_ctxs(|m| {
            let a = m.get(&h)?;
            for x in a.asms.iter().skip(1).filter(|x| !x.base.starts_with("C:\\Windows\\WinSxS")) {
                if x.m.files.iter().any(|f| matches!(&key, Key::Str(s) if eq_nocase(s, &w16s(&f.name)))) {
                    return Some(format!("{}{}", x.base, base));
                }
            }
            None
        });
        if let Some(p) = hit {
            return Some(p);
        }
    }
    None
}

/// Where a DLL of a system assembly of the active context is (C:\\Windows\\WinSxS\\..\\name), when
/// one lists it.
pub fn builtin_dll_path(name: &str) -> Option<String> {
    let base = name.rsplit(['\\', '/']).next()?.to_ascii_lowercase();
    let base = if base.contains('.') { base } else { format!("{base}.dll") };
    for h in [active(), process_default()] {
        if !valid(h) {
            continue;
        }
        let hit = with_ctxs(|m| {
            let a = m.get(&h)?;
            a.asms.iter().skip(1).filter(|x| x.base.starts_with("C:\\Windows\\WinSxS")).find(|x| x.m.files.iter().any(|f| f.name.eq_ignore_ascii_case(&base))).map(|x| format!("{}{base}", x.base))
        });
        if hit.is_some() {
            return hit;
        }
    }
    None
}

pub fn init() {
    for dll in ["kernel32", "kernelbase"] {
        implement(dll, "CreateActCtxA", CreateActCtxA);
        implement(dll, "CreateActCtxW", CreateActCtxW);
        implement(dll, "ActivateActCtx", ActivateActCtx);
        implement(dll, "DeactivateActCtx", DeactivateActCtx);
        implement(dll, "GetCurrentActCtx", GetCurrentActCtx);
        implement(dll, "ReleaseActCtx", ReleaseActCtx);
        implement(dll, "AddRefActCtx", AddRefActCtx);
        implement(dll, "ZombifyActCtx", ZombifyActCtx);
        implement(dll, "QueryActCtxW", QueryActCtxW);
        implement(dll, "QueryActCtxSettingsW", QueryActCtxSettingsW);
        implement(dll, "FindActCtxSectionStringA", FindActCtxSectionStringA);
        implement(dll, "FindActCtxSectionStringW", FindActCtxSectionStringW);
        implement(dll, "FindActCtxSectionGuid", FindActCtxSectionGuid);
    }
    reg!("ntdll"; RtlCreateActivationContext, RtlActivateActivationContext, RtlActivateActivationContextEx, RtlDeactivateActivationContext,
        RtlActivateActivationContextUnsafeFast, RtlDeactivateActivationContextUnsafeFast, RtlFreeThreadActivationContextStack,
        RtlGetActiveActivationContext, RtlIsActivationContextActive, RtlZombifyActivationContext, RtlQueryInformationActivationContext,
        RtlQueryInformationActiveActivationContext, RtlQueryActivationContextApplicationSettings, RtlFindActivationContextSectionString,
        RtlFindActivationContextSectionGuid);
    implement("ntdll", "RtlReleaseActivationContext", ReleaseActCtx);
    implement("ntdll", "RtlAddRefActivationContext", AddRefActCtx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn asm(body: &str) -> String {
        format!(r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">{body}</assembly>"#)
    }
    const ID: &str = r#"<assemblyIdentity version="2.0.0.1" name="Sake.App" type="win32"/>"#;

    #[test]
    fn reads_identity_files_and_dependencies() {
        let m = parse_manifest(
            asm(&format!(
                r#"{ID}<file name="a.dll"><windowClass>One</windowClass><windowClass versioned="no">Two</windowClass></file>
                <dependency><dependentAssembly><assemblyIdentity type="win32" name="dep" version="1.2.3.4" processorArchitecture="x86"/></dependentAssembly></dependency>"#
            ))
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(m.id.name, "Sake.App");
        assert_eq!(m.id.version, Some([2, 0, 0, 1]));
        assert_eq!(m.id.encoded(), r#"Sake.App,type="win32",version="2.0.0.1""#);
        assert_eq!(m.files[0].classes, vec![("One".to_string(), true), ("Two".to_string(), false)]);
        assert_eq!(m.deps[0].id.encoded(), r#"dep,processorArchitecture="x86",type="win32",version="1.2.3.4""#);
    }

    #[test]
    fn spaces_around_equals_and_comments() {
        let t = r#"<?xml version="1.0"?><!-- c --><assembly xmlns = "urn:schemas-microsoft-com:asm.v1" manifestVersion = '1.0'><assemblyIdentity name = "x" version = "1.0.0.0"/></assembly><!-- end -->"#;
        assert_eq!(parse_manifest(t.as_bytes()).unwrap().id.name, "x");
    }

    #[test]
    fn utf16_needs_its_bom() {
        let t = asm(ID);
        let le: Vec<u8> = [0xfeffu16].iter().chain(&t.encode_utf16().collect::<Vec<_>>()).flat_map(|c| c.to_le_bytes()).collect();
        let be: Vec<u8> = [0xfeffu16].iter().chain(&t.encode_utf16().collect::<Vec<_>>()).flat_map(|c| c.to_be_bytes()).collect();
        assert!(parse_manifest(&le).is_some());
        assert!(parse_manifest(&be).is_some());
        assert!(parse_manifest(&le[2..]).is_none());
    }

    #[test]
    fn refuses_what_windows_refuses() {
        let bad = [
            format!(r#"<assembly manifestVersion="1.0">{ID}</assembly>"#),
            format!(r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1">{ID}</assembly>"#),
            format!(r#"<assembly x="1" xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">{ID}</assembly>"#),
            asm(&format!("{ID}<unknown/>")),
            format!("{}<extra/>", asm(ID)),
            format!(r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v9" manifestVersion="1.0">{ID}</assembly>"#),
            asm(&format!(r#"{ID}<file name="a.dll" hash="0123456789abcdef0123456789abcdef0123456" hashalg="SHA1"/>"#)),
            asm(&format!("{ID}<file></file>")),
            asm(&format!(
                r#"{ID}<trustInfo xmlns="urn:schemas-microsoft-com:asm.v2"><security><requestedPrivileges><requestedExecutionLevel level="asInvoker"/><requestedExecutionLevel uiAccess="true"/></requestedPrivileges></security></trustInfo>"#
            )),
            asm(&format!(r#"{ID}<file name="a.dll"><activatableClass name="A.B" threadingModel="both"/></file>"#)),
            asm(r#"<assemblyIdentity name="x" version="1.0"/>"#),
            asm("<assemblyIdentity name=\"x\" version=\"1.0.0.0\">"),
        ];
        for b in &bad {
            assert!(parse_manifest(b.as_bytes()).is_none(), "{b}");
        }
        assert!(parse_manifest(asm("").as_bytes()).is_some());
        assert!(parse_manifest(br#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0"/>"#).is_some());
        assert!(parse_manifest(asm(&format!("{ID}<dependency/>")).as_bytes()).is_some());
    }

    #[test]
    fn run_level_compat_and_settings() {
        let m = parse_manifest(
            asm(&format!(
                r#"{ID}<asmv2:trustInfo xmlns:asmv2="urn:schemas-microsoft-com:asm.v2"><asmv2:security><asmv2:requestedPrivileges>
                <asmv2:requestedExecutionLevel level="REQUIREADMINISTRATOR" uiAccess="TRUE"/></asmv2:requestedPrivileges></asmv2:security></asmv2:trustInfo>
                <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1"><application>
                <supportedOS Id="{{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}}"/><maxversiontested Id="10.0.18358"/></application></compatibility>
                <application xmlns="urn:schemas-microsoft-com:asm.v3"><windowsSettings>
                <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true</dpiAware></windowsSettings></application>
                <application xmlns="urn:schemas-microsoft-com:asm.v3"><windowsSettings>
                <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitor</dpiAwareness></windowsSettings></application>"#
            ))
            .as_bytes(),
        )
        .unwrap();
        assert_eq!((m.run_level, m.ui_access), (3, true));
        assert_eq!(m.compat, vec![Compat::Os(parse_guid("8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a").unwrap()), Compat::MaxVersion(0x000a_0000_47b6_0000)]);
        assert_eq!(m.settings, vec![(SETTINGS_NS[0].to_string(), "dpiAware".to_string(), "true".to_string())]);
        // an undeclared prefix: its elements are someone else's, the manifest stays good
        let m = parse_manifest(asm(&format!("{ID}<asmv3:application><asmv3:windowsSettings><dpiAware>true</dpiAware></asmv3:windowsSettings></asmv3:application>")).as_bytes()).unwrap();
        assert!(m.settings.is_empty());
    }

    #[test]
    fn guid_parse() {
        let g = parse_guid("{12345678-1234-5678-1234-111122223333}").unwrap();
        assert_eq!(g, [0x78, 0x56, 0x34, 0x12, 0x34, 0x12, 0x78, 0x56, 0x12, 0x34, 0x11, 0x11, 0x22, 0x22, 0x33, 0x33]);
        assert!(parse_guid("{12345678-1234-5678-1234-11112222333}").is_none());
        assert!(parse_guid("zz345678-1234-5678-1234-111122223333").is_none());
    }

    #[test]
    fn satisfies_matches_exact_versions_and_wildcards() {
        let def = Ident { name: "Dep".into(), version: Some([1, 2, 3, 4]), arch: Some("x86".into()), lang: Some("fr-FR".into()), ..Default::default() };
        let r = |v: [u16; 4], arch: &str, lang: Option<&str>| Ident { name: "dep".into(), version: Some(v), arch: Some(arch.into()), lang: lang.map(Into::into), ..Default::default() };
        assert!(def.satisfies(&r([1, 2, 3, 4], "X86", None)));
        assert!(def.satisfies(&r([1, 2, 3, 4], "*", Some("*"))));
        assert!(def.satisfies(&r([1, 2, 3, 4], "x86", Some("fr-fr"))));
        assert!(!def.satisfies(&r([1, 2, 3, 5], "x86", None)));
        assert!(!def.satisfies(&r([1, 2, 3, 4], "amd64", None)));
        assert!(!def.satisfies(&r([1, 2, 3, 4], "x86", Some("de-DE"))));
    }

    fn wstr_at(sec: &[u8], at: u32, bytes: u32) -> String {
        String::from_utf16(&sec[at as usize..(at + bytes) as usize].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn window_class_and_dll_sections() {
        let dep = parse_manifest(
            asm(r#"<assemblyIdentity name="d" version="6.5.4.3"/><file name="lib.dll"><windowClass>Cls</windowClass><windowClass versioned="no">Plain</windowClass></file><file name="other.dll"/>"#)
                .as_bytes(),
        )
        .unwrap();
        let root = parse_manifest(asm(ID).as_bytes()).unwrap();
        let asms = [root, dep];
        let sec = make_section(SEC_WINDOW_CLASS_REDIRECTION, &asms).unwrap();
        assert_eq!(get32(&sec, 0), Some(STRSECTION_MAGIC));
        assert_eq!(get32(&sec, 4), Some(44));
        assert_eq!(get32(&sec, 20), Some(2));
        let (d, len, roster) = section_find(&sec, &Key::Str(w16s("CLS"))).unwrap();
        assert_eq!(roster, 2);
        assert_eq!(get32(&sec, d), Some(24));
        let nl = get32(&sec, d + 8).unwrap();
        assert_eq!(get32(&sec, d + 12), Some(24));
        assert_eq!(wstr_at(&sec, d + 24, nl), "6.5.4.3!Cls");
        let ml = get32(&sec, d + 16).unwrap();
        let mo = get32(&sec, d + 20).unwrap();
        assert_eq!(mo, d + 24 + nl + 2);
        assert_eq!(wstr_at(&sec, mo, ml), "lib.dll");
        assert_eq!(len, 24 + nl + ml + 4);
        let (d, _, _) = section_find(&sec, &Key::Str(w16s("Plain"))).unwrap();
        assert_eq!(wstr_at(&sec, d + 24, get32(&sec, d + 8).unwrap()), "Plain");
        assert!(section_find(&sec, &Key::Str(w16s("Nope"))).is_none());
        assert_eq!(section_global(&sec).1, 0);

        let sec = make_section(SEC_DLL_REDIRECTION, &asms).unwrap();
        assert_eq!(get32(&sec, 20), Some(2));
        let (d, len, roster) = section_find(&sec, &Key::Str(w16s("OTHER.DLL"))).unwrap();
        assert_eq!((len, roster), (20, 2));
        assert_eq!([0, 4, 8, 12, 16].map(|k| get32(&sec, d + k).unwrap()), [20, 2, 0, 0, 0]);
    }

    #[test]
    fn com_progid_and_typelib_sections() {
        let m = parse_manifest(
            asm(r#"<assemblyIdentity name="c" version="1.0.0.0"/><file name="srv.dll">
              <comClass clsid="{12345678-1234-5678-1234-111122223333}" tlbid="{99999999-8888-7777-6666-555555555555}" threadingModel="Both" progid="A.B" miscStatus="cantlinkinside"><progid>A.B.1</progid></comClass>
              <typelib tlbid="{99999999-8888-7777-6666-555555555555}" version="2.1" helpdir="hlp" resourceid="409" flags="hidden,control"/></file>
              <clrClass clsid="{22345678-1234-5678-1234-111122223333}" name="N" runtimeVersion="v4" progid="clr.p" threadingModel="Neutral"/>"#)
                .as_bytes(),
        )
        .unwrap();
        let asms = [m];
        let com = make_section(SEC_COM_SERVER_REDIRECTION, &asms).unwrap();
        assert_eq!(get32(&com, 0), Some(GUIDSECTION_MAGIC));
        assert_eq!(get32(&com, 4), Some(40));
        let clsid = parse_guid("12345678-1234-5678-1234-111122223333").unwrap();
        let (d, len, _) = section_find(&com, &Key::Guid(clsid)).unwrap();
        assert_eq!(get32(&com, d), Some(120));
        assert_eq!(get32(&com, d + 4), Some(1 << 8));
        assert_eq!(get32(&com, d + 8), Some(4));
        let alias: Guid = com[d as usize + 28..d as usize + 44].try_into().unwrap();
        assert_eq!(section_find(&com, &Key::Guid(alias)).unwrap().0, d);
        let (pl, po) = (get32(&com, d + 84).unwrap(), get32(&com, d + 88).unwrap());
        assert_eq!(wstr_at(&com, d + po, pl), "A.B");
        assert_eq!(len, 120 + pl + 2);
        let (nl, no) = (get32(&com, d + 76).unwrap(), get32(&com, d + 80).unwrap());
        assert_eq!(wstr_at(&com, no, nl), "srv.dll");
        let (go, gl) = section_global(&com);
        assert!(no >= go && no < go + gl);
        // the CLR class: its data after the header, mscoree in both spellings
        let (d, len, _) = section_find(&com, &Key::Guid(parse_guid("22345678-1234-5678-1234-111122223333").unwrap())).unwrap();
        let (cl, co) = (get32(&com, d + 92).unwrap(), get32(&com, d + 96).unwrap());
        assert_eq!(co, 120);
        assert_eq!(get32(&com, d + co), Some(44));
        assert_eq!(get32(&com, d + co + 8), Some(2));
        assert_eq!(wstr_at(&com, get32(&com, d + co + 16).unwrap(), 22), "mscoree.dll");
        assert_eq!(wstr_at(&com, get32(&com, d + 80).unwrap(), 22), "MSCOREE.DLL");
        assert_eq!(get32(&com, d + 88), Some(120 + cl));
        assert_eq!(len, 120 + cl + get32(&com, d + 84).unwrap() + 2);

        let prog = make_section(SEC_COM_PROGID_REDIRECTION, &asms).unwrap();
        for p in ["A.B", "a.b.1"] {
            let (d, len, _) = section_find(&prog, &Key::Str(w16s(p))).unwrap();
            assert_eq!(len, 12);
            let at = get32(&prog, d + 8).unwrap() as usize;
            assert_eq!(&prog[at..at + 16], &alias);
        }
        // the second class's ProgID leads to the second class
        let (d, _, _) = section_find(&prog, &Key::Str(w16s("clr.p"))).unwrap();
        let at = get32(&prog, d + 8).unwrap() as usize;
        let a2: Guid = prog[at..at + 16].try_into().unwrap();
        let (cd, _, _) = section_find(&com, &Key::Guid(a2)).unwrap();
        assert_eq!(&com[cd as usize + 12..cd as usize + 28], &parse_guid("22345678-1234-5678-1234-111122223333").unwrap());
        let tl = make_section(SEC_COM_TYPE_LIBRARY_REDIRECTION, &asms).unwrap();
        let (d, _, _) = section_find(&tl, &Key::Guid(parse_guid("99999999-8888-7777-6666-555555555555").unwrap())).unwrap();
        assert_eq!(get32(&tl, d), Some(32));
        assert_eq!(u16::from_le_bytes([tl[d as usize + 16], tl[d as usize + 17]]), 0x409);
        assert_eq!(u16::from_le_bytes([tl[d as usize + 18], tl[d as usize + 19]]), 6);
        assert_eq!(get32(&tl, d + 20), Some(6));
        assert_eq!(wstr_at(&tl, d + 32, 6), "hlp");
        assert_eq!(u16::from_le_bytes([tl[d as usize + 28], tl[d as usize + 29]]), 2);
    }

    #[test]
    fn builtin_common_controls() {
        let r = Ident { name: "Microsoft.Windows.Common-Controls".into(), version: Some([6, 0, 0, 0]), arch: Some("*".into()), token: Some(CC_TOKEN.into()), lang: Some("*".into()), typ: Some("win32".into()) };
        let (m, path) = builtin(&r).unwrap();
        assert_eq!(m.files[0].name, "comctl32.dll");
        assert!(m.files[0].classes.iter().any(|c| c.0 == "SysListView32"));
        assert!(path.starts_with("C:\\Windows\\WinSxS\\manifests\\"));
        assert!(builtin(&Ident { token: Some("0000000000000000".into()), ..r.clone() }).is_none());
        assert!(builtin(&Ident { name: "nothing".into(), ..r }).is_none());
    }

    fn arb_name() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9_.]{0,12}"
    }

    proptest! {
        /// Every key of a built string section is found (in any case), with its data and roster
        /// index; a key that is not there is not.
        #[test]
        fn string_sections_find_their_keys(keys in proptest::collection::btree_set(arb_name(), 0..24), probe in arb_name()) {
            let keys: Vec<String> = keys.into_iter().fold(Vec::new(), |mut v: Vec<String>, k| { if !v.iter().any(|x| x.eq_ignore_ascii_case(&k)) { v.push(k) } v });
            let ents: Vec<Ent> = keys.iter().enumerate().map(|(i, k)| {
                let tag = i as u32;
                Ent { keys: vec![Key::Str(w16s(k))], roster: tag + 1, data: Box::new(move |off, _| { let mut v = Vec::new(); p32(&mut v, tag); p32(&mut v, off); v }) }
            }).collect();
            let sec = build_section(false, &ents, b"glob");
            prop_assert_eq!(get32(&sec, 20), Some(keys.len() as u32));
            for (i, k) in keys.iter().enumerate() {
                let (d, len, roster) = section_find(&sec, &Key::Str(w16s(&k.to_ascii_uppercase()))).unwrap();
                prop_assert_eq!((len, roster), (8, i as u32 + 1));
                prop_assert_eq!(get32(&sec, d), Some(i as u32));
                prop_assert_eq!(get32(&sec, d + 4), Some(d));
            }
            if !keys.iter().any(|k| k.eq_ignore_ascii_case(&probe)) {
                prop_assert!(section_find(&sec, &Key::Str(w16s(&probe))).is_none());
            }
            let (go, gl) = section_global(&sec);
            prop_assert_eq!(&sec[go as usize..(go + gl) as usize], b"glob");
        }

        #[test]
        fn guid_sections_find_their_keys(guids in proptest::collection::btree_set(any::<[u8; 16]>(), 0..16)) {
            let guids: Vec<Guid> = guids.into_iter().collect();
            let ents: Vec<Ent> = guids.iter().enumerate().map(|(i, g)| Ent { keys: vec![Key::Guid(*g)], roster: i as u32, data: Box::new(move |_, _| vec![i as u8; i + 1]) }).collect();
            let sec = build_section(true, &ents, &[]);
            for (i, g) in guids.iter().enumerate() {
                let (d, len, roster) = section_find(&sec, &Key::Guid(*g)).unwrap();
                prop_assert_eq!((len, roster), (i as u32 + 1, i as u32));
                prop_assert!(sec[d as usize..(d + len) as usize].iter().all(|&b| b == i as u8));
            }
        }

        /// The XML reader never panics, and a section never reads outside itself.
        #[test]
        fn readers_take_any_bytes(b in proptest::collection::vec(any::<u8>(), 0..400)) {
            let _ = parse_manifest(&b);
            let _ = section_find(&b, &Key::Str(w16s("x")));
            let _ = section_find(&b, &Key::Guid([0; 16]));
        }

        /// Generated documents read back as written: names, attributes (escaped), text, nesting.
        #[test]
        fn xml_reads_back_what_was_written(tree in arb_elem(3)) {
            let text = write_elem(&tree);
            let (e, _) = xml::parse(&text).unwrap();
            prop_assert_eq!(strip(&e), tree);
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    struct T {
        name: String,
        attrs: Vec<(String, String)>,
        text: String,
        kids: Vec<T>,
    }

    fn arb_elem(depth: u32) -> BoxedStrategy<T> {
        let leaf = (arb_name(), proptest::collection::btree_map("[a-z]{1,6}", "[ -~]{0,10}", 0..4), "[A-Za-z0-9 <>&\"']{0,10}");
        if depth == 0 {
            return leaf.prop_map(|(name, a, text)| T { name, attrs: a.into_iter().collect(), text, kids: vec![] }).boxed();
        }
        (leaf, proptest::collection::vec(arb_elem(depth - 1), 0..3))
            .prop_map(|((name, a, text), kids)| T { name, attrs: a.into_iter().collect(), text, kids })
            .boxed()
    }

    fn esc(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
    }

    fn write_elem(t: &T) -> String {
        let mut s = format!("<{}", t.name);
        for (k, v) in &t.attrs {
            s.push_str(&format!(" {k}=\"{}\"", esc(v)));
        }
        s.push('>');
        s.push_str(&esc(&t.text));
        for k in &t.kids {
            s.push_str(&write_elem(k));
        }
        s.push_str(&format!("</{}>", t.name));
        s
    }

    fn strip(e: &xml::Elem) -> T {
        let name = if e.ns.starts_with('?') { format!("{}:{}", &e.ns[1..], e.name) } else { e.name.clone() };
        let attrs = e.attrs.iter().map(|a| (if a.0.starts_with('?') { format!("{}:{}", &a.0[1..], a.1) } else { a.1.clone() }, a.2.clone())).collect();
        T { name, attrs, text: e.text.clone(), kids: e.children.iter().map(strip).collect() }
    }
}
