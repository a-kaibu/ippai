//! The C layouts of the D3D8, D3D9, D3D11 and DXGI headers (mingw-w64's, laid out by clang for
//! i686 and x86_64: tests/replay/layouts.py writes layouts.txt): every struct's members with
//! their offsets on both, every interface's methods with their vtable index and parameter types,
//! the entry points' parameters. The converter packs arguments by them.

use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    /// an integer or enum of this many bytes
    Int(u8),
    F4,
    F8,
    F10,
    Void,
    /// a returned HRESULT
    Hr,
    /// a function pointer
    Fn,
    /// an HWND
    Hwnd,
    /// a struct or union
    Rec(String),
    /// a COM interface
    Obj(String),
    Ptr(Box<Ty>),
    Arr(u32, Box<Ty>),
    /// a bit field of this many bits
    Bits(u8),
}

#[derive(Debug)]
pub struct Member {
    /// on i686, on x86_64
    pub off: [u32; 2],
    /// "-" for an anonymous member
    pub name: String,
    pub ty: Ty,
}

#[derive(Debug)]
pub struct Record {
    pub size: [u32; 2],
    pub members: Vec<Member>,
}

#[derive(Debug)]
pub struct Method {
    pub index: u32,
    pub name: String,
    pub ret: Ty,
    /// (the object itself left out)
    pub params: Vec<Ty>,
}

#[derive(Debug)]
pub struct Interface {
    pub base: Option<String>,
    /// the methods it adds to its base's
    pub methods: Vec<Method>,
}

#[derive(Debug, Default)]
pub struct Layouts {
    pub records: HashMap<String, Record>,
    pub interfaces: HashMap<String, Interface>,
    pub functions: HashMap<String, (Ty, Vec<Ty>)>,
}

pub fn parse_ty(s: &str) -> Result<Ty, String> {
    Ok(match s {
        "i1" => Ty::Int(1),
        "i2" => Ty::Int(2),
        "i4" => Ty::Int(4),
        "i8" => Ty::Int(8),
        "f4" => Ty::F4,
        "f8" => Ty::F8,
        "f10" => Ty::F10,
        "v" => Ty::Void,
        "hr" => Ty::Hr,
        "p" => Ty::Fn,
        "H" => Ty::Hwnd,
        _ => {
            if let Some(r) = s.strip_prefix('*') {
                Ty::Ptr(Box::new(parse_ty(r)?))
            } else if let Some(r) = s.strip_prefix("S:") {
                Ty::Rec(r.into())
            } else if let Some(r) = s.strip_prefix("O:") {
                Ty::Obj(r.into())
            } else if let Some(r) = s.strip_prefix('b') {
                Ty::Bits(r.parse().map_err(|_| format!("layouts: type {s}"))?)
            } else if let Some(r) = s.strip_prefix('[') {
                let (n, t) = r.split_once(']').ok_or(format!("layouts: type {s}"))?;
                Ty::Arr(n.parse().map_err(|_| format!("layouts: type {s}"))?, Box::new(parse_ty(t)?))
            } else {
                return Err(format!("layouts: type {s}"));
            }
        }
    })
}

fn params(s: Option<&str>) -> Result<Vec<Ty>, String> {
    match s {
        None | Some("") => Ok(vec![]),
        Some(s) => s.split(',').map(parse_ty).collect(),
    }
}

impl Layouts {
    pub fn parse(text: &str) -> Result<Layouts, String> {
        let mut l = Layouts::default();
        let mut rec: Option<String> = None;
        let mut iface: Option<String> = None;
        for line in text.lines() {
            let w: Vec<&str> = line.split_whitespace().collect();
            let bad = || format!("layouts: {line:?}");
            if let Some(body) = line.strip_prefix(' ') {
                let w: Vec<&str> = body.split(' ').collect();
                if let Some(r) = &rec {
                    let [o32, o64, name, ty] = w[..] else { return Err(bad()) };
                    let m = Member { off: [o32.parse().map_err(|_| bad())?, o64.parse().map_err(|_| bad())?], name: name.into(), ty: parse_ty(ty)? };
                    l.records.get_mut(r).unwrap().members.push(m);
                } else if let Some(i) = &iface {
                    if w.len() < 3 {
                        return Err(bad());
                    }
                    let m = Method { index: w[0].parse().map_err(|_| bad())?, name: w[1].into(), ret: parse_ty(w[2])?, params: params(w.get(3).copied())? };
                    l.interfaces.get_mut(i).unwrap().methods.push(m);
                } else {
                    return Err(bad());
                }
                continue;
            }
            rec = None;
            iface = None;
            match w.first() {
                Some(&"R") if w.len() == 6 => {
                    l.records.insert(w[1].into(), Record { size: [w[2].parse().map_err(|_| bad())?, w[3].parse().map_err(|_| bad())?], members: vec![] });
                    rec = Some(w[1].into());
                }
                Some(&"V") if w.len() == 3 => {
                    l.interfaces.insert(w[1].into(), Interface { base: (w[2] != "-").then(|| w[2].into()), methods: vec![] });
                    iface = Some(w[1].into());
                }
                Some(&"F") if w.len() >= 3 => {
                    l.functions.insert(w[1].into(), (parse_ty(w[2])?, params(w.get(3).copied())?));
                }
                _ => return Err(bad()),
            }
        }
        Ok(l)
    }

    /// the headers' own (layouts.txt)
    pub fn get() -> &'static Layouts {
        static L: OnceLock<Layouts> = OnceLock::new();
        L.get_or_init(|| Layouts::parse(include_str!("layouts.txt")).expect("layouts.txt"))
    }

    /// a method of an interface or of one of its bases
    pub fn method(&self, iface: &str, name: &str) -> Option<&Method> {
        let mut i = self.interfaces.get(iface)?;
        loop {
            if let Some(m) = i.methods.iter().find(|m| m.name == name) {
                return Some(m);
            }
            i = self.interfaces.get(i.base.as_deref()?)?;
        }
    }

    /// a type's size (`arch`: 0 for i686, 1 for x86_64)
    pub fn size(&self, ty: &Ty, arch: usize) -> Result<u32, String> {
        Ok(match ty {
            Ty::Int(w) => *w as u32,
            Ty::F4 => 4,
            Ty::F8 => 8,
            Ty::F10 => [12, 16][arch],
            Ty::Void => 1,
            Ty::Hr => 4,
            Ty::Fn | Ty::Hwnd | Ty::Ptr(_) => [4, 8][arch],
            Ty::Rec(r) => self.records.get(r).ok_or(format!("no layout of {r}"))?.size[arch],
            Ty::Obj(o) => return Err(format!("an interface by value ({o})")),
            Ty::Arr(n, t) => n * self.size(t, arch)?,
            Ty::Bits(_) => return Err("a bit field's size".into()),
        })
    }
}
