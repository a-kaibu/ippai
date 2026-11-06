//! HLSL syntax: tokens (pp.rs) to the tree of ast.rs.

use super::ast::*;
use super::pp::{Tok, Token};

type R<T> = Result<T, String>;

pub struct Parser {
    t: Vec<Token>,
    p: usize,
    pub prog: Program,
    typedefs: Vec<(String, Ty)>,
    /// #pragma pack_matrix
    pack: Option<bool>,
}

fn scalar_base(s: &str) -> Option<B> {
    Some(match s {
        "bool" => B::Bool,
        "int" | "dword" | "min16int" | "min12int" | "int16_t" | "int32_t" => B::Int,
        "uint" | "min16uint" | "uint16_t" | "uint32_t" | "unsigned" => B::Uint,
        "float" | "half" | "double" | "min16float" | "min10float" | "float16_t" | "float32_t" | "float64_t" => B::Float,
        _ => return None,
    })
}

/// float, float3, float4x4, half2...
pub fn builtin_numeric(s: &str) -> Option<Ty> {
    let digits = s.trim_end_matches(|c: char| c.is_ascii_digit() || c == 'x');
    // (min16float's 16 is part of the name)
    for cut in [s.len(), digits.len()] {
        let (name, rest) = s.split_at(cut);
        let Some(b) = scalar_base(name) else { continue };
        let r = rest.as_bytes();
        return match r {
            [] => Some(Ty::Scalar(b)),
            [n] if (b'1'..=b'4').contains(n) => Some(Ty::vec(b, (n - b'0') as usize)),
            [rr, b'x', c] if (b'1'..=b'4').contains(rr) && (b'1'..=b'4').contains(c) => Some(Ty::Mat(b, rr - b'0', c - b'0')),
            _ => None,
        };
    }
    None
}

fn tex_dim(s: &str) -> Option<Dim> {
    Some(match s {
        "Buffer" => Dim::Buffer,
        "Texture1D" => Dim::T1D,
        "Texture1DArray" => Dim::T1DArray,
        "Texture2D" | "texture" | "Texture" | "texture2D" => Dim::T2D,
        "Texture2DArray" => Dim::T2DArray,
        "Texture2DMS" => Dim::T2DMS,
        "Texture2DMSArray" => Dim::T2DMSArray,
        "Texture3D" | "texture3D" => Dim::T3D,
        "TextureCube" | "textureCUBE" => Dim::Cube,
        "TextureCubeArray" => Dim::CubeArray,
        _ => return None,
    })
}

const MODIFIERS: &[&str] = &[
    "static", "const", "uniform", "extern", "shared", "groupshared", "volatile", "row_major", "column_major", "inline", "precise",
    "linear", "centroid", "nointerpolation", "noperspective", "sample", "in", "out", "inout", "snorm", "unorm", "export",
];

impl Parser {
    pub fn new(t: Vec<Token>) -> Self {
        Parser { t, p: 0, prog: Program::default(), typedefs: Vec::new(), pack: None }
    }

    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.p).map(|t| &t.t)
    }
    fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.t.get(self.p + n).map(|t| &t.t)
    }
    fn line(&self) -> u32 {
        self.t.get(self.p).or(self.t.last()).map_or(0, |t| t.line)
    }
    fn err<T>(&self, m: &str) -> R<T> {
        let near = match self.peek() {
            Some(Tok::Ident(s)) => s.clone(),
            Some(Tok::Punct(p)) => p.to_string(),
            Some(t) => format!("{t:?}"),
            None => "end of file".into(),
        };
        Err(format!("line {}: {m} (near '{near}')", self.line()))
    }
    fn is(&self, p: &str) -> bool {
        matches!(self.peek(), Some(Tok::Punct(x)) if *x == p)
    }
    fn is_id(&self, s: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(x)) if x == s)
    }
    fn eat(&mut self, p: &str) -> bool {
        if self.is(p) {
            self.p += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, p: &str) -> R<()> {
        if self.eat(p) {
            Ok(())
        } else {
            self.err(&format!("expected '{p}'"))
        }
    }
    fn ident(&mut self) -> R<String> {
        match self.peek() {
            Some(Tok::Ident(s)) => {
                let s = s.clone();
                self.p += 1;
                Ok(s)
            }
            _ => self.err("expected a name"),
        }
    }

    /// Skips a balanced group starting at the current `open`.
    fn skip_group(&mut self, open: &str, close: &str) -> R<()> {
        let mut d = 0;
        loop {
            match self.peek() {
                None => return self.err(&format!("unterminated '{open}'")),
                Some(Tok::Punct(x)) if *x == open => d += 1,
                Some(Tok::Punct(x)) if *x == close => {
                    d -= 1;
                    if d == 0 {
                        self.p += 1;
                        return Ok(());
                    }
                }
                _ => {}
            }
            self.p += 1;
        }
    }

    fn is_type_name(&self, s: &str) -> bool {
        builtin_numeric(s).is_some()
            || tex_dim(s).is_some()
            || matches!(
                s,
                "void" | "vector" | "matrix" | "string" | "sampler" | "sampler1D" | "sampler2D" | "sampler3D" | "samplerCUBE"
                    | "SamplerState" | "SamplerComparisonState" | "sampler_state" | "struct"
            )
            || self.prog.structs.iter().any(|x| x.name == s)
            || self.typedefs.iter().any(|x| x.0 == s)
    }

    fn template_num(&mut self) -> R<u32> {
        match self.peek() {
            Some(Tok::Int(v, _)) => {
                let v = *v as u32;
                self.p += 1;
                Ok(v)
            }
            _ => self.err("expected a number"),
        }
    }

    /// A type at the current position.
    fn ty(&mut self) -> R<Ty> {
        let name = self.ident()?;
        if let Some(t) = builtin_numeric(&name) {
            return Ok(t);
        }
        if let Some(t) = self.typedefs.iter().find(|x| x.0 == name) {
            return Ok(t.1.clone());
        }
        if let Some(i) = self.prog.structs.iter().position(|x| x.name == name) {
            return Ok(Ty::Struct(i));
        }
        if let Some(d) = tex_dim(&name) {
            let mut el = Ty::Vec(B::Float, 4);
            if self.eat("<") {
                el = self.ty()?;
                if self.eat(",") {
                    self.template_num()?;
                }
                self.expect(">")?;
            }
            if name.starts_with('t') && name != "texture" {
                // (texture2D.. of effects)
            }
            return Ok(Ty::Tex(d, Box::new(el)));
        }
        Ok(match name.as_str() {
            "void" => Ty::Void,
            "string" => Ty::Str,
            "vector" => {
                if self.eat("<") {
                    let b = self.ty()?.base();
                    self.expect(",")?;
                    let n = self.template_num()?;
                    self.expect(">")?;
                    Ty::vec(b, n as usize)
                } else {
                    Ty::Vec(B::Float, 4)
                }
            }
            "matrix" => {
                if self.eat("<") {
                    let b = self.ty()?.base();
                    self.expect(",")?;
                    let r = self.template_num()?;
                    self.expect(",")?;
                    let c = self.template_num()?;
                    self.expect(">")?;
                    Ty::Mat(b, r as u8, c as u8)
                } else {
                    Ty::Mat(B::Float, 4, 4)
                }
            }
            "sampler" | "sampler2D" => Ty::Legacy(Dim::T2D),
            "sampler1D" => Ty::Legacy(Dim::T1D),
            "sampler3D" => Ty::Legacy(Dim::T3D),
            "samplerCUBE" => Ty::Legacy(Dim::Cube),
            "SamplerState" => Ty::Sampler(false),
            "SamplerComparisonState" => Ty::Sampler(true),
            "struct" => {
                // an inline struct definition
                let i = self.struct_def()?;
                Ty::Struct(i)
            }
            _ => {
                self.p -= 1;
                return self.err("unknown type");
            }
        })
    }

    /// struct [Name] { fields } (after the keyword); its index.
    fn struct_def(&mut self) -> R<usize> {
        let name = if let Some(Tok::Ident(_)) = self.peek() { self.ident()? } else { format!("$anon{}", self.prog.structs.len()) };
        if !self.is("{") {
            // (a use: struct Foo x;)
            return self.prog.structs.iter().position(|s| s.name == name).ok_or(format!("line {}: unknown struct {name}", self.line()));
        }
        self.expect("{")?;
        let mut fields = Vec::new();
        while !self.eat("}") {
            let m = self.modifiers()?;
            let base = self.ty()?;
            loop {
                let fname = self.ident()?;
                let ty = self.array_suffix(base.clone())?;
                let mut v = Var { name: fname, ..Default::default() };
                self.post_decl(&mut v)?;
                fields.push(Field { name: v.name, ty, sem: v.sem, interp: m.interp, row_major: m.row_major.or(self.pack) });
                if !self.eat(",") {
                    break;
                }
            }
            self.expect(";")?;
        }
        self.prog.structs.push(Struct { name, fields });
        Ok(self.prog.structs.len() - 1)
    }

    fn array_suffix(&mut self, mut ty: Ty) -> R<Ty> {
        let mut dims = Vec::new();
        while self.eat("[") {
            if self.eat("]") {
                dims.push(0);
                continue;
            }
            let e = self.expr()?;
            self.expect("]")?;
            let n = const_int(&e).ok_or(format!("line {}: array size is not a constant", self.line()))?;
            dims.push(n as u32);
        }
        for n in dims.into_iter().rev() {
            ty = Ty::Array(Box::new(ty), n);
        }
        Ok(ty)
    }

    /// storage and interpolation modifiers into a Var's fields
    fn modifiers(&mut self) -> R<Var> {
        let mut v = Var { is_in: true, ..Default::default() };
        let mut explicit_in = false;
        while let Some(Tok::Ident(s)) = self.peek() {
            if !MODIFIERS.contains(&s.as_str()) {
                break;
            }
            match s.as_str() {
                "static" => v.is_static = true,
                "const" => v.is_const = true,
                "uniform" => v.uniform = true,
                "row_major" => v.row_major = Some(true),
                "column_major" => v.row_major = Some(false),
                "centroid" => v.interp = if v.interp == Interp::NoPersp { Interp::NoPerspCentroid } else { Interp::Centroid },
                "nointerpolation" => v.interp = Interp::NoInterp,
                "noperspective" => v.interp = if v.interp == Interp::Centroid { Interp::NoPerspCentroid } else { Interp::NoPersp },
                "sample" => v.interp = Interp::Sample,
                "in" => explicit_in = true,
                "out" => {
                    v.is_out = true;
                    if !explicit_in {
                        v.is_in = false;
                    }
                }
                "inout" => {
                    v.is_out = true;
                    v.is_in = true;
                }
                _ => {}
            }
            self.p += 1;
        }
        Ok(v)
    }

    /// : SEMANTIC, : register(..), : packoffset(..), <annotations>, after a declared name
    fn post_decl(&mut self, v: &mut Var) -> R<()> {
        loop {
            if self.is("<") {
                self.skip_group("<", ">")?;
                continue;
            }
            if !self.eat(":") {
                return Ok(());
            }
            let n = self.ident()?;
            match n.as_str() {
                "register" => {
                    self.expect("(")?;
                    let mut last = None;
                    while !self.eat(")") {
                        if let Some(Tok::Ident(s)) = self.peek() {
                            let s = s.clone();
                            let c = s.chars().next().unwrap_or(' ').to_ascii_lowercase();
                            if let Ok(i) = s[1..].parse::<u32>() {
                                if !s.starts_with("space") {
                                    last = Some((c, i));
                                }
                            }
                        }
                        self.p += 1;
                    }
                    v.reg = last;
                }
                "packoffset" => {
                    self.expect("(")?;
                    let r = self.ident()?;
                    let reg: u32 = r[1..].parse().map_err(|_| format!("line {}: bad packoffset", self.line()))?;
                    let mut comp = 0;
                    if self.eat(".") {
                        let c = self.ident()?;
                        comp = match c.as_str() {
                            "y" | "g" => 1,
                            "z" | "b" => 2,
                            "w" | "a" => 3,
                            _ => 0,
                        };
                    }
                    self.expect(")")?;
                    v.packoffset = Some(reg * 16 + comp * 4);
                }
                _ => v.sem = Some(n),
            }
        }
    }

    pub fn program(&mut self) -> R<()> {
        while self.peek().is_some() {
            self.top()?;
        }
        Ok(())
    }

    fn top(&mut self) -> R<()> {
        if let Some(Tok::PackMatrix(r)) = self.peek() {
            self.pack = Some(*r);
            self.p += 1;
            return Ok(());
        }
        if self.eat(";") {
            return Ok(());
        }
        if self.is("[") {
            self.skip_group("[", "]")?;
            return Ok(());
        }
        let line = self.line();
        if let Some(Tok::Ident(s)) = self.peek() {
            match s.as_str() {
                "cbuffer" | "tbuffer" => {
                    self.p += 1;
                    let name = self.ident()?;
                    let mut v = Var::default();
                    self.post_decl(&mut v)?;
                    self.expect("{")?;
                    let mut vars = Vec::new();
                    while !self.eat("}") {
                        if let Some(Tok::PackMatrix(r)) = self.peek() {
                            self.pack = Some(*r);
                            self.p += 1;
                            continue;
                        }
                        vars.extend(self.decl()?);
                    }
                    self.eat(";");
                    self.prog.cbufs.push(CBuf { name, reg: v.reg.map(|r| r.1), vars });
                    return Ok(());
                }
                "struct" if matches!(self.peek_at(2), Some(Tok::Punct("{"))) || matches!(self.peek_at(1), Some(Tok::Punct("{"))) => {
                    self.p += 1;
                    let i = self.struct_def()?;
                    if self.eat(";") {
                        return Ok(());
                    }
                    // struct S {..} var;
                    let names = self.declarators(Ty::Struct(i), Var { is_in: true, ..Default::default() }, line)?;
                    self.prog.globals.extend(names);
                    return Ok(());
                }
                "typedef" => {
                    self.p += 1;
                    let _ = self.modifiers()?;
                    let t = self.ty()?;
                    let n = self.ident()?;
                    let t = self.array_suffix(t)?;
                    self.expect(";")?;
                    self.typedefs.push((n, t));
                    return Ok(());
                }
                "technique" | "technique9" | "technique10" | "technique11" | "pass" | "fxgroup" => {
                    while !self.is("{") {
                        if self.peek().is_none() {
                            return self.err("bad technique");
                        }
                        self.p += 1;
                    }
                    self.skip_group("{", "}")?;
                    self.eat(";");
                    return Ok(());
                }
                _ => {}
            }
        }
        let m = self.modifiers()?;
        let ty = self.ty()?;
        let name = self.ident()?;
        if self.is("(") {
            return self.function(ty, name);
        }
        self.p -= 1;
        let vs = self.declarators(ty, m, line)?;
        self.prog.globals.extend(vs);
        Ok(())
    }

    fn function(&mut self, ret: Ty, name: String) -> R<()> {
        self.expect("(")?;
        let mut params = Vec::new();
        if !self.eat(")") {
            if self.is_id("void") && matches!(self.peek_at(1), Some(Tok::Punct(")"))) {
                self.p += 2;
            } else {
                loop {
                    let mut v = self.modifiers()?;
                    // (geometry shader primitive types)
                    if matches!(self.peek(), Some(Tok::Ident(s)) if matches!(s.as_str(), "point" | "line" | "triangle" | "lineadj" | "triangleadj")) {
                        return self.err("geometry shaders are not supported");
                    }
                    let t = self.ty()?;
                    v.name = self.ident()?;
                    v.ty = Some(self.array_suffix(t)?);
                    v.line = self.line();
                    self.post_decl(&mut v)?;
                    if self.eat("=") {
                        v.init = Some(self.assign_expr()?);
                    }
                    if v.row_major.is_none() {
                        v.row_major = self.pack;
                    }
                    params.push(v);
                    if !self.eat(",") {
                        break;
                    }
                }
                self.expect(")")?;
            }
        }
        let mut rv = Var::default();
        self.post_decl(&mut rv)?;
        let body = if self.eat(";") { None } else { Some(self.block()?) };
        if body.is_none() && self.prog.funcs.iter().any(|f| f.name == name && f.params.len() == params.len()) {
            return Ok(());
        }
        // (a definition after a prototype replaces it)
        self.prog.funcs.retain(|f| !(f.name == name && f.params.len() == params.len() && f.body.is_none()));
        self.prog.funcs.push(Func { name, ret, ret_sem: rv.sem, params, body });
        Ok(())
    }

    /// name[n] : SEM = init, name2 ... ;
    fn declarators(&mut self, base: Ty, m: Var, line: u32) -> R<Vec<Var>> {
        let mut out = Vec::new();
        loop {
            let mut v = m.clone();
            v.line = line;
            v.name = self.ident()?;
            v.ty = Some(self.array_suffix(base.clone())?);
            self.post_decl(&mut v)?;
            if v.row_major.is_none() {
                v.row_major = self.pack;
            }
            if self.eat("=") {
                if self.is_id("sampler_state") || self.is_id("compile") {
                    while !self.is(";") && !self.is(",") {
                        if self.is("{") {
                            self.skip_group("{", "}")?;
                        } else if self.peek().is_none() {
                            return self.err("unterminated declaration");
                        } else {
                            self.p += 1;
                        }
                    }
                } else {
                    v.init = Some(self.assign_expr()?);
                }
            }
            // SamplerState s { Filter = ...; };
            if self.is("{") {
                self.skip_group("{", "}")?;
            }
            out.push(v);
            if !self.eat(",") {
                break;
            }
        }
        self.expect(";")?;
        Ok(out)
    }

    /// A declaration statement (or cbuffer member).
    fn decl(&mut self) -> R<Vec<Var>> {
        let line = self.line();
        let m = self.modifiers()?;
        let ty = self.ty()?;
        if self.eat(";") {
            // (struct S { .. };)
            return Ok(Vec::new());
        }
        self.declarators(ty, m, line)
    }

    fn block(&mut self) -> R<Vec<Stmt>> {
        self.expect("{")?;
        let mut v = Vec::new();
        while !self.eat("}") {
            if self.peek().is_none() {
                return self.err("missing '}'");
            }
            v.push(self.stmt()?);
        }
        Ok(v)
    }

    fn stmt(&mut self) -> R<Stmt> {
        while self.is("[") {
            self.skip_group("[", "]")?;
        }
        if let Some(Tok::PackMatrix(r)) = self.peek() {
            self.pack = Some(*r);
            self.p += 1;
            return Ok(Stmt::Block(Vec::new()));
        }
        if self.is("{") {
            return Ok(Stmt::Block(self.block()?));
        }
        if self.eat(";") {
            return Ok(Stmt::Block(Vec::new()));
        }
        let line = self.line();
        if let Some(Tok::Ident(s)) = self.peek() {
            match s.as_str() {
                "if" => {
                    self.p += 1;
                    self.expect("(")?;
                    let c = self.expr()?;
                    self.expect(")")?;
                    let a = self.stmt()?;
                    let b = if self.is_id("else") {
                        self.p += 1;
                        Some(Box::new(self.stmt()?))
                    } else {
                        None
                    };
                    return Ok(Stmt::If(c, Box::new(a), b));
                }
                "for" => {
                    self.p += 1;
                    self.expect("(")?;
                    let init = if self.eat(";") {
                        None
                    } else if self.starts_decl() {
                        Some(Box::new(Stmt::Decl(self.decl()?)))
                    } else {
                        let e = self.expr()?;
                        self.expect(";")?;
                        Some(Box::new(Stmt::Expr(e)))
                    };
                    let cond = if self.is(";") { None } else { Some(self.expr()?) };
                    self.expect(";")?;
                    let step = if self.is(")") { None } else { Some(self.expr()?) };
                    self.expect(")")?;
                    let body = self.stmt()?;
                    return Ok(Stmt::For(init, cond, step, Box::new(body)));
                }
                "while" => {
                    self.p += 1;
                    self.expect("(")?;
                    let c = self.expr()?;
                    self.expect(")")?;
                    return Ok(Stmt::While(c, Box::new(self.stmt()?)));
                }
                "do" => {
                    self.p += 1;
                    let body = self.stmt()?;
                    if !self.is_id("while") {
                        return self.err("expected 'while'");
                    }
                    self.p += 1;
                    self.expect("(")?;
                    let c = self.expr()?;
                    self.expect(")")?;
                    self.expect(";")?;
                    return Ok(Stmt::Do(Box::new(body), c));
                }
                "switch" => {
                    self.p += 1;
                    self.expect("(")?;
                    let v = self.expr()?;
                    self.expect(")")?;
                    self.expect("{")?;
                    let mut cases: Vec<(Vec<Option<Expr>>, Vec<Stmt>)> = Vec::new();
                    while !self.eat("}") {
                        if self.is_id("case") || self.is_id("default") {
                            let mut labels = Vec::new();
                            while self.is_id("case") || self.is_id("default") {
                                if self.is_id("case") {
                                    self.p += 1;
                                    labels.push(Some(self.expr()?));
                                } else {
                                    self.p += 1;
                                    labels.push(None);
                                }
                                self.expect(":")?;
                            }
                            cases.push((labels, Vec::new()));
                        } else {
                            let s = self.stmt()?;
                            match cases.last_mut() {
                                Some(c) => c.1.push(s),
                                None => return self.err("statement before case"),
                            }
                        }
                    }
                    return Ok(Stmt::Switch(v, cases));
                }
                "break" => {
                    self.p += 1;
                    self.expect(";")?;
                    return Ok(Stmt::Break);
                }
                "continue" => {
                    self.p += 1;
                    self.expect(";")?;
                    return Ok(Stmt::Continue);
                }
                "discard" => {
                    self.p += 1;
                    self.expect(";")?;
                    return Ok(Stmt::Discard);
                }
                "return" => {
                    self.p += 1;
                    let e = if self.is(";") { None } else { Some(self.expr()?) };
                    self.expect(";")?;
                    return Ok(Stmt::Return(e, line));
                }
                "struct" if matches!(self.peek_at(2), Some(Tok::Punct("{"))) => {
                    self.p += 1;
                    let i = self.struct_def()?;
                    if self.eat(";") {
                        return Ok(Stmt::Block(Vec::new()));
                    }
                    return Ok(Stmt::Decl(self.declarators(Ty::Struct(i), Var { is_in: true, ..Default::default() }, line)?));
                }
                _ => {}
            }
        }
        if self.starts_decl() {
            return Ok(Stmt::Decl(self.decl()?));
        }
        let e = self.expr()?;
        self.expect(";")?;
        Ok(Stmt::Expr(e))
    }

    fn starts_decl(&self) -> bool {
        match self.peek() {
            Some(Tok::Ident(s)) if MODIFIERS.contains(&s.as_str()) => true,
            // (a type then a name: float4(..) is an expression)
            Some(Tok::Ident(s)) if self.is_type_name(s) => {
                let mut i = 1;
                if matches!(self.peek_at(1), Some(Tok::Punct("<"))) {
                    while !matches!(self.peek_at(i), Some(Tok::Punct(">")) | None) {
                        i += 1;
                    }
                    i += 1;
                }
                matches!(self.peek_at(i), Some(Tok::Ident(_)))
            }
            _ => false,
        }
    }

    // ---- expressions ----

    pub fn expr(&mut self) -> R<Expr> {
        let mut e = self.assign_expr()?;
        while self.eat(",") {
            let b = self.assign_expr()?;
            e = Expr::Comma(Box::new(e), Box::new(b));
        }
        Ok(e)
    }

    fn assign_expr(&mut self) -> R<Expr> {
        if self.is("{") {
            self.p += 1;
            let mut v = Vec::new();
            while !self.eat("}") {
                v.push(self.assign_expr()?);
                if !self.eat(",") {
                    self.expect("}")?;
                    break;
                }
            }
            return Ok(Expr::List(v));
        }
        let l = self.cond_expr()?;
        let op = match self.peek() {
            Some(Tok::Punct(p)) => match *p {
                "=" => Some(None),
                "+=" => Some(Some(Bin::Add)),
                "-=" => Some(Some(Bin::Sub)),
                "*=" => Some(Some(Bin::Mul)),
                "/=" => Some(Some(Bin::Div)),
                "%=" => Some(Some(Bin::Mod)),
                "<<=" => Some(Some(Bin::Shl)),
                ">>=" => Some(Some(Bin::Shr)),
                "&=" => Some(Some(Bin::And)),
                "|=" => Some(Some(Bin::Or)),
                "^=" => Some(Some(Bin::Xor)),
                _ => None,
            },
            _ => None,
        };
        if let Some(op) = op {
            self.p += 1;
            let r = self.assign_expr()?;
            return Ok(Expr::Assign(op, Box::new(l), Box::new(r)));
        }
        Ok(l)
    }

    fn cond_expr(&mut self) -> R<Expr> {
        let c = self.bin_expr(0)?;
        if self.eat("?") {
            let a = self.assign_expr()?;
            self.expect(":")?;
            let b = self.assign_expr()?;
            return Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    fn bin_expr(&mut self, min: u32) -> R<Expr> {
        let mut l = self.unary()?;
        loop {
            let Some(Tok::Punct(p)) = self.peek() else { break };
            let (prec, op) = match *p {
                "||" => (1, Bin::LOr),
                "&&" => (2, Bin::LAnd),
                "|" => (3, Bin::Or),
                "^" => (4, Bin::Xor),
                "&" => (5, Bin::And),
                "==" => (6, Bin::Eq),
                "!=" => (6, Bin::Ne),
                "<" => (7, Bin::Lt),
                ">" => (7, Bin::Gt),
                "<=" => (7, Bin::Le),
                ">=" => (7, Bin::Ge),
                "<<" => (8, Bin::Shl),
                ">>" => (8, Bin::Shr),
                "+" => (9, Bin::Add),
                "-" => (9, Bin::Sub),
                "*" => (10, Bin::Mul),
                "/" => (10, Bin::Div),
                "%" => (10, Bin::Mod),
                _ => break,
            };
            if prec < min {
                break;
            }
            self.p += 1;
            let r = self.bin_expr(prec + 1)?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn unary(&mut self) -> R<Expr> {
        if let Some(Tok::Punct(p)) = self.peek() {
            let op = match *p {
                "-" => Some(Un::Neg),
                "!" => Some(Un::Not),
                "~" => Some(Un::BitNot),
                "++" => Some(Un::PreInc),
                "--" => Some(Un::PreDec),
                "+" => {
                    self.p += 1;
                    return self.unary();
                }
                "(" => {
                    // a cast: (type) expr
                    if let Some(Tok::Ident(s)) = self.peek_at(1) {
                        if self.is_type_name(s) && s != "struct" {
                            let save = self.p;
                            self.p += 1;
                            if let Ok(t) = self.ty() {
                                if let Ok(t) = self.array_suffix(t) {
                                    if self.eat(")") {
                                        let e = self.unary()?;
                                        return Ok(Expr::Cast(t, Box::new(e)));
                                    }
                                }
                            }
                            self.p = save;
                        }
                    }
                    None
                }
                _ => None,
            };
            if let Some(op) = op {
                self.p += 1;
                let e = self.unary()?;
                return Ok(Expr::Un(op, Box::new(e)));
            }
        }
        self.postfix()
    }

    fn postfix(&mut self) -> R<Expr> {
        let mut e = self.primary()?;
        loop {
            if self.eat(".") {
                let n = self.ident()?;
                if self.is("(") {
                    let args = self.args()?;
                    e = Expr::Method(Box::new(e), n, args);
                } else {
                    e = Expr::Member(Box::new(e), n);
                }
            } else if self.eat("[") {
                let i = self.expr()?;
                self.expect("]")?;
                e = Expr::Index(Box::new(e), Box::new(i));
            } else if self.eat("++") {
                e = Expr::Un(Un::PostInc, Box::new(e));
            } else if self.eat("--") {
                e = Expr::Un(Un::PostDec, Box::new(e));
            } else {
                return Ok(e);
            }
        }
    }

    fn args(&mut self) -> R<Vec<Expr>> {
        self.expect("(")?;
        let mut v = Vec::new();
        if self.eat(")") {
            return Ok(v);
        }
        loop {
            v.push(self.assign_expr()?);
            if !self.eat(",") {
                break;
            }
        }
        self.expect(")")?;
        Ok(v)
    }

    fn primary(&mut self) -> R<Expr> {
        let line = self.line();
        match self.peek().cloned() {
            Some(Tok::Int(v, u)) => {
                self.p += 1;
                Ok(Expr::Int(v as i64, u))
            }
            Some(Tok::Float(f)) => {
                self.p += 1;
                Ok(Expr::Float(f))
            }
            Some(Tok::Punct("(")) => {
                self.p += 1;
                let e = self.expr()?;
                self.expect(")")?;
                Ok(e)
            }
            Some(Tok::Ident(s)) => {
                if s == "true" || s == "false" {
                    self.p += 1;
                    return Ok(Expr::Bool(s == "true"));
                }
                if self.is_type_name(&s) && matches!(self.peek_at(1), Some(Tok::Punct("(" | "<"))) && s != "struct" {
                    let t = self.ty()?;
                    let args = self.args()?;
                    return Ok(Expr::Construct(t, args));
                }
                self.p += 1;
                if self.is("(") {
                    let args = self.args()?;
                    return Ok(Expr::Call(s, args, line));
                }
                // (Foo::Bar: static members are not supported; the name alone)
                Ok(Expr::Var(s, line))
            }
            Some(Tok::Str(_)) => {
                self.p += 1;
                Ok(Expr::Int(0, false))
            }
            _ => self.err("expected an expression"),
        }
    }
}

/// The value of a constant integer expression (array sizes).
pub fn const_int(e: &Expr) -> Option<i64> {
    Some(match e {
        Expr::Int(v, _) => *v,
        Expr::Float(f) => *f as i64,
        Expr::Bin(op, a, b) => {
            let (a, b) = (const_int(a)?, const_int(b)?);
            match op {
                Bin::Add => a + b,
                Bin::Sub => a - b,
                Bin::Mul => a * b,
                Bin::Div => a.checked_div(b)?,
                Bin::Mod => a.checked_rem(b)?,
                Bin::Shl => a << b,
                Bin::Shr => a >> b,
                Bin::And => a & b,
                Bin::Or => a | b,
                _ => return None,
            }
        }
        Expr::Un(Un::Neg, a) => -const_int(a)?,
        Expr::Cast(_, a) => const_int(a)?,
        _ => return None,
    })
}
