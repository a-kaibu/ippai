//! MSVC's C++ name undecoration (msvcrt's __unDName / __unDNameEx, dbghelp's
//! UnDecorateSymbolName), written from the decoration grammar: a decorated name is
//! `?` name qualifiers `@` and an encoding (function, data, vtable, RTTI, thunk), its types the
//! one-letter codes, back-references (digits) into the names seen so far and, in a function's
//! parameters, into the parameter types seen so far. Template arguments have their own tables.
//!
//! The text follows the undecorator Windows ships, quirks and all (pointers to functions print
//! `(__cdecl*name)`, a member function's qualifiers `const ` with a space after them, `>>`
//! written `> >`), as tests/winref/probes/undname.c checks against UnDecorateSymbolName.
//!
//! A name this does not understand undecorates to itself, as Windows' does.

#![allow(non_snake_case)]

/// UNDNAME_* flags
pub const NO_LEADING_UNDERSCORES: u32 = 0x1;
pub const NO_MS_KEYWORDS: u32 = 0x2;
pub const NO_FUNCTION_RETURNS: u32 = 0x4;
pub const NO_ALLOCATION_LANGUAGE: u32 = 0x10;
pub const NO_MS_THISTYPE: u32 = 0x20;
pub const NO_CV_THISTYPE: u32 = 0x40;
pub const NO_ACCESS_SPECIFIERS: u32 = 0x80;
pub const NO_THROW_SIGNATURES: u32 = 0x100;
pub const NO_MEMBER_TYPE: u32 = 0x200;
pub const NAME_ONLY: u32 = 0x1000;
/// (UNDNAME_NO_ARGUMENTS for functions; with it a name not starting with `?` is a type alone,
/// as type_info::name asks)
pub const TYPE_ONLY: u32 = 0x2000;

/// The longest argument list (template or function) an undecoration builds, in bytes
pub const OUT_MAX: usize = 0x1_0000;

/// `mangled` undecorated as `flags` ask, or None when it is not a decorated name this knows.
pub fn undname(mangled: &str, flags: u32) -> Option<String> {
    let mut p = P { s: mangled.as_bytes(), i: 0, flags, names: Vec::new(), params: Vec::new(), depth: 0 };
    let out = if flags & TYPE_ONLY != 0 && !mangled.starts_with("??") && !(mangled.starts_with('?') && !mangled[1..].starts_with(['A', 'B', 'C', 'D'])) {
        p.datatype()?
    } else {
        p.symbol()?
    };
    (p.i == p.s.len()).then_some(out)
}

#[derive(Clone, Debug)]
enum Ty {
    /// a type with its cv-qualifiers (bit 0 const, bit 1 volatile)
    Base { name: String, cv: u8, unaligned: bool },
    /// a pointer or reference: `mark` is `*`, `&`, `&&`, `^`, `%` or `Class::*`; `tail` what
    /// a variable's storage class adds after its own qualifiers
    Ptr { mark: String, cv: u8, ptr64: bool, restrict: bool, tail: String, ret: bool, to: Box<Ty> },
    /// a function (pointed to, or a template argument): its return type, parameters already
    /// written out, and qualifiers of `this` (a pointer to member function: `class`)
    Fn { cc: String, ret: Option<Box<Ty>>, args: String, this_q: String, class: Option<String> },
    Array { cv: u8, dims: String, elem: Box<Ty> },
}

fn cv_word(cv: u8) -> &'static str {
    ["", "const", "volatile", "const volatile"][cv as usize & 3]
}

/// `s` followed by a declarator: no space when it starts with one already
fn join(s: String, decl: &str) -> String {
    if decl.is_empty() {
        s
    } else if decl.starts_with(' ') {
        s + decl
    } else {
        s + " " + decl
    }
}

fn sep(decl: &str) -> String {
    if decl.is_empty() || decl.starts_with(' ') {
        decl.to_string()
    } else {
        format!(" {decl}")
    }
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
    flags: u32,
    /// names a digit refers back to
    names: Vec<String>,
    /// parameter types a digit refers back to (in a parameter list)
    params: Vec<Ty>,
    depth: u32,
}

/// what a function's encoding letter says: its access (0 none, 1 private, 2 protected, 3
/// public), kind (0 plain, 1 static, 2 virtual, 3 adjustor thunk) and whether it has `this`
fn member(c: u8) -> Option<(u8, u8, bool)> {
    Some(match c {
        b'A' | b'B' => (1, 0, true),
        b'C' | b'D' => (1, 1, false),
        b'E' | b'F' => (1, 2, true),
        b'G' | b'H' => (1, 3, true),
        b'I' | b'J' => (2, 0, true),
        b'K' | b'L' => (2, 1, false),
        b'M' | b'N' => (2, 2, true),
        b'O' | b'P' => (2, 3, true),
        b'Q' | b'R' => (3, 0, true),
        b'S' | b'T' => (3, 1, false),
        b'U' | b'V' => (3, 2, true),
        b'W' | b'X' => (3, 3, true),
        b'Y' | b'Z' => (0, 0, false),
        _ => return None,
    })
}

const ACCESS: [&str; 4] = ["", "private: ", "protected: ", "public: "];

/// `?X` operator codes, `?_X` and `?__X` ones
fn operator(c: u8) -> Option<&'static str> {
    Some(match c {
        b'2' => "operator new",
        b'3' => "operator delete",
        b'4' => "operator=",
        b'5' => "operator>>",
        b'6' => "operator<<",
        b'7' => "operator!",
        b'8' => "operator==",
        b'9' => "operator!=",
        b'A' => "operator[]",
        b'C' => "operator->",
        b'D' => "operator*",
        b'E' => "operator++",
        b'F' => "operator--",
        b'G' => "operator-",
        b'H' => "operator+",
        b'I' => "operator&",
        b'J' => "operator->*",
        b'K' => "operator/",
        b'L' => "operator%",
        b'M' => "operator<",
        b'N' => "operator<=",
        b'O' => "operator>",
        b'P' => "operator>=",
        b'Q' => "operator,",
        b'R' => "operator()",
        b'S' => "operator~",
        b'T' => "operator^",
        b'U' => "operator|",
        b'V' => "operator&&",
        b'W' => "operator||",
        b'X' => "operator*=",
        b'Y' => "operator+=",
        b'Z' => "operator-=",
        _ => return None,
    })
}
fn operator_(c: u8) -> Option<&'static str> {
    Some(match c {
        b'0' => "operator/=",
        b'1' => "operator%=",
        b'2' => "operator>>=",
        b'3' => "operator<<=",
        b'4' => "operator&=",
        b'5' => "operator|=",
        b'6' => "operator^=",
        b'7' => "`vftable'",
        b'8' => "`vbtable'",
        b'9' => "`vcall'",
        b'A' => "`typeof'",
        b'B' => "`local static guard'",
        b'C' => "`string'",
        b'D' => "`vbase destructor'",
        b'E' => "`vector deleting destructor'",
        b'F' => "`default constructor closure'",
        b'G' => "`scalar deleting destructor'",
        b'H' => "`vector constructor iterator'",
        b'I' => "`vector destructor iterator'",
        b'J' => "`vector vbase constructor iterator'",
        b'K' => "`virtual displacement map'",
        b'L' => "`eh vector constructor iterator'",
        b'M' => "`eh vector destructor iterator'",
        b'N' => "`eh vector vbase constructor iterator'",
        b'O' => "`copy constructor closure'",
        b'S' => "`local vftable'",
        b'T' => "`local vftable constructor closure'",
        b'U' => "operator new[]",
        b'V' => "operator delete[]",
        b'X' => "`placement delete closure'",
        b'Y' => "`placement delete[] closure'",
        _ => return None,
    })
}
fn operator__(c: u8) -> Option<&'static str> {
    Some(match c {
        b'A' => "`managed vector constructor iterator'",
        b'B' => "`managed vector destructor iterator'",
        b'C' => "`eh vector copy constructor iterator'",
        b'D' => "`eh vector vbase copy constructor iterator'",
        b'G' => "`vector copy constructor iterator'",
        b'H' => "`vector vbase copy constructor iterator'",
        b'I' => "`managed vector copy constructor iterator'",
        b'J' => "`local static thread guard'",
        b'L' => "operator co_await",
        b'M' => "operator<=>",
        _ => return None,
    })
}

/// the symbol's own name, as the encoding needs to know it
enum Unq {
    Name(String),
    /// a constructor, destructor or conversion, with its template arguments (`<...>` or "")
    Ctor(String),
    Dtor(String),
    Cast(String),
}

impl<'a> P<'a> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn peek_at(&self, k: usize) -> Option<u8> {
        self.s.get(self.i + k).copied()
    }
    fn next(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        Some(c)
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn eat_str(&mut self, s: &str) -> bool {
        if self.s[self.i..].starts_with(s.as_bytes()) {
            self.i += s.len();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, c: u8) -> Option<()> {
        self.eat(c).then_some(())
    }
    fn flag(&self, f: u32) -> bool {
        self.flags & f != 0
    }

    /// an MS keyword (`__cdecl`, `__ptr64`): without its underscores, or not at all, as the
    /// flags say
    fn kw(&self, k: &str) -> String {
        if self.flag(NO_MS_KEYWORDS) {
            String::new()
        } else if self.flag(NO_LEADING_UNDERSCORES) {
            k.trim_start_matches('_').to_string()
        } else {
            k.to_string()
        }
    }

    /// a number: a digit is 1..10, else hex digits A..P ended by `@`; `?` makes it negative
    fn number(&mut self) -> Option<(bool, u64)> {
        let neg = self.eat(b'?');
        let c = self.next()?;
        if c.is_ascii_digit() {
            return Some((neg, (c - b'0') as u64 + 1));
        }
        let mut v: u64 = 0;
        let mut c = c;
        loop {
            match c {
                b'A'..=b'P' => v = v.checked_mul(16)? | (c - b'A') as u64,
                b'@' => return Some((neg, v)),
                _ => return None,
            }
            c = self.next()?;
        }
    }
    fn signed(&mut self) -> Option<String> {
        let (neg, v) = self.number()?;
        Some(if neg { format!("-{v}") } else { v.to_string() })
    }
    fn unsigned(&mut self) -> Option<String> {
        let (neg, v) = self.number()?;
        Some(if neg { (v as u32).wrapping_neg().to_string() } else { v.to_string() })
    }

    fn remember(&mut self, s: &str) {
        if self.names.len() < 10 && !self.names.iter().any(|n| n == s) {
            self.names.push(s.to_string());
        }
    }

    /// an identifier ended by `@`
    fn ident(&mut self) -> Option<String> {
        let start = self.i;
        loop {
            let c = self.next()?;
            if c == b'@' {
                break;
            }
            if !(c.is_ascii_alphanumeric() || matches!(c, b'_' | b'$' | b'<' | b'>' | b'-') || c >= 0x80) {
                return None;
            }
        }
        if self.i - 1 == start {
            return None;
        }
        Some(String::from_utf8_lossy(&self.s[start..self.i - 1]).into_owned())
    }

    /// `<args>` of a template, its name `base` the first name its own table refers back to
    fn template_args(&mut self, base: Option<&str>) -> Option<String> {
        // (a template argument naming a template naming a template...: counted with the types,
        // as nothing else on that path is)
        self.depth += 1;
        if self.depth > 64 {
            return None;
        }
        let r = self.template_args_inner(base);
        self.depth -= 1;
        r
    }
    fn template_args_inner(&mut self, base: Option<&str>) -> Option<String> {
        let names = std::mem::take(&mut self.names);
        let params = std::mem::take(&mut self.params);
        if let Some(b) = base {
            self.remember(b);
        }
        let r = self.template_list();
        self.names = names;
        self.params = params;
        let args = r?;
        Some(if args.ends_with('>') { format!("<{args} >") } else { format!("<{args}>") })
    }
    fn template_list(&mut self) -> Option<String> {
        let mut out: Vec<String> = Vec::new();
        loop {
            match self.peek() {
                None => break,
                Some(b'@') => {
                    self.i += 1;
                    break;
                }
                _ => {}
            }
            if let Some(a) = self.template_arg()? {
                out.push(a);
            }
            // (back-references to whole template names double the text per level: a few
            // hundred bytes could ask for gigabytes; Windows' undecorator has a bounded buffer)
            if out.iter().map(|a| a.len() + 1).sum::<usize>() > OUT_MAX {
                return None;
            }
        }
        Some(out.join(","))
    }
    /// one template argument (None: an empty pack)
    fn template_arg(&mut self) -> Option<Option<String>> {
        if self.eat_str("$$V") || self.eat_str("$$Z") || self.eat_str("$S") {
            return Some(None);
        }
        if self.peek() == Some(b'$') && self.peek_at(1) != Some(b'$') {
            self.i += 1;
            let k = self.next()?;
            return Some(Some(match k {
                b'0' => self.signed()?,
                b'1' => format!("&{}", self.nested_name()?),
                b'E' => self.nested_name()?,
                b'D' => format!("`template-parameter{}'", self.signed()?),
                b'Q' => format!("`non-type-template-parameter{}'", self.signed()?),
                b'F' => format!("{{{},{}}}", self.signed()?, self.signed()?),
                b'G' => format!("{{{},{},{}}}", self.signed()?, self.signed()?, self.signed()?),
                b'H' => format!("{{{},{}}}", self.nested_name()?, self.signed()?),
                b'I' => format!("{{{},{},{}}}", self.nested_name()?, self.signed()?, self.signed()?),
                b'J' => format!("{{{},{},{},{}}}", self.nested_name()?, self.signed()?, self.signed()?, self.signed()?),
                b'2' => {
                    let (_, m) = self.number()?;
                    let e = self.signed()?;
                    format!("{m}e{e}")
                }
                _ => return None,
            }));
        }
        if self.peek() == Some(b'?') && self.peek_at(1) != Some(b'A') && self.peek_at(1) != Some(b'B') {
            self.i += 1;
            return Some(Some(format!("`template-parameter-{}'", self.signed()?)));
        }
        if self.eat(b'X') {
            return Some(Some("void".into()));
        }
        let t = self.ty(At::Plain)?;
        Some(Some(self.render(&t, "")))
    }
    /// a whole decorated name used as a template argument: its qualified name
    fn nested_name(&mut self) -> Option<String> {
        self.depth += 1;
        if self.depth > 64 {
            return None;
        }
        let r = self.nested_name_inner();
        self.depth -= 1;
        r
    }
    fn nested_name_inner(&mut self) -> Option<String> {
        self.expect(b'?')?;
        let (unq, quals) = self.full_name(false)?;
        let name = self.qualified(&unq, &quals);
        self.encoding_skip()?;
        Some(name)
    }
    /// past the encoding of a symbol named in a template argument
    fn encoding_skip(&mut self) -> Option<()> {
        let c = self.next()?;
        match c {
            b'0'..=b'4' => {
                let t = self.ty(At::Plain)?;
                self.storage(t)?;
            }
            _ => {
                self.i -= 1;
                let (acc, kind, this) = member(c)?;
                self.i += 1;
                let _ = (acc, kind);
                self.function_rest(this)?;
            }
        }
        Some(())
    }

    /// the symbol's own name and its qualifiers (innermost first)
    fn full_name(&mut self, typename: bool) -> Option<(Unq, Vec<String>)> {
        let unq = self.unqualified(typename)?;
        let quals = self.qualifiers()?;
        Some((unq, quals))
    }
    fn unqualified(&mut self, typename: bool) -> Option<Unq> {
        if self.eat_str("?$") {
            // a template: a function's (`??$name@`, `??$?0`) or one alone
            if self.peek() == Some(b'?') {
                self.i += 1;
                let special = self.special()?;
                let args = self.template_args(None)?;
                return Some(match special {
                    Unq::Name(n) => Unq::Name(n + &args),
                    Unq::Ctor(_) => Unq::Ctor(args),
                    Unq::Dtor(_) => Unq::Dtor(args),
                    Unq::Cast(_) => Unq::Cast(args),
                });
            }
            let n = self.ident()?;
            let full = n.clone() + &self.template_args(Some(&n))?;
            // (a type's name is remembered whole; a symbol's own template name is not)
            if typename {
                self.remember(&full);
            }
            return Some(Unq::Name(full));
        }
        if self.eat(b'?') {
            return self.special();
        }
        if let Some(c) = self.peek().filter(u8::is_ascii_digit) {
            self.i += 1;
            return Some(Unq::Name(self.names.get((c - b'0') as usize)?.clone()));
        }
        let n = self.ident()?;
        self.remember(&n);
        Some(Unq::Name(n))
    }
    /// after `??`: a constructor, destructor, operator or compiler-made name
    fn special(&mut self) -> Option<Unq> {
        let c = self.next()?;
        Some(match c {
            b'0' => Unq::Ctor(String::new()),
            b'1' => Unq::Dtor(String::new()),
            b'B' => Unq::Cast(String::new()),
            b'_' => {
                if self.eat(b'_') {
                    let k = self.next()?;
                    match k {
                        b'E' | b'F' => {
                            let what = if k == b'E' { "dynamic initializer" } else { "dynamic atexit destructor" };
                            let n = if self.peek() == Some(b'?') {
                                let n = self.nested_name()?;
                                self.expect(b'@')?;
                                n
                            } else {
                                self.ident()?
                            };
                            Unq::Name(format!("`{what} for '{n}''"))
                        }
                        b'K' => Unq::Name(format!("operator \"\" {}", self.ident()?)),
                        _ => Unq::Name(operator__(k)?.into()),
                    }
                } else if self.eat(b'R') {
                    let k = self.next()?;
                    Unq::Name(match k {
                        b'0' => return None, // (a type descriptor: `symbol` takes it)
                        b'1' => {
                            let v = [self.signed()?, self.signed()?, self.signed()?, self.signed()?];
                            format!("`RTTI Base Class Descriptor at ({},{},{},{})'", v[0], v[1], v[2], v[3])
                        }
                        b'2' => "`RTTI Base Class Array'".into(),
                        b'3' => "`RTTI Class Hierarchy Descriptor'".into(),
                        b'4' => "`RTTI Complete Object Locator'".into(),
                        _ => return None,
                    })
                } else {
                    Unq::Name(operator_(self.next()?)?.into())
                }
            }
            c => Unq::Name(operator(c)?.into()),
        })
    }
    /// qualifiers up to `@`: namespaces, classes (templates remembered whole), anonymous
    /// namespaces, and the locally scoped `` `function'::`N' ``
    fn qualifiers(&mut self) -> Option<Vec<String>> {
        let mut q = Vec::new();
        loop {
            match self.peek() {
                None => return None,
                Some(b'@') => {
                    self.i += 1;
                    return Some(q);
                }
                Some(c) if c.is_ascii_digit() => {
                    self.i += 1;
                    q.push(self.names.get((c - b'0') as usize)?.clone());
                }
                Some(b'?') => {
                    self.i += 1;
                    match self.peek()? {
                        b'$' => {
                            self.i += 1;
                            let n = self.ident()?;
                            let full = n.clone() + &self.template_args(Some(&n))?;
                            self.remember(&full);
                            q.push(full);
                        }
                        b'A' => {
                            self.i += 1;
                            self.ident()?;
                            let n = "`anonymous namespace'".to_string();
                            self.remember(&n);
                            q.push(n);
                        }
                        b'?' => {
                            // a whole symbol: `` `symbol' ``
                            let s = self.inner_symbol()?;
                            q.push(format!("`{s}'"));
                        }
                        _ => {
                            let n = self.signed()?;
                            q.push(format!("`{n}'"));
                            if self.eat(b'?') {
                                let s = self.inner_symbol()?;
                                q.push(format!("`{s}'"));
                            }
                        }
                    }
                }
                Some(_) => {
                    let n = self.ident()?;
                    self.remember(&n);
                    q.push(n);
                }
            }
        }
    }
    /// a decorated name inside a name (its tables shared with the outer one)
    fn inner_symbol(&mut self) -> Option<String> {
        self.depth += 1;
        if self.depth > 32 {
            return None;
        }
        let r = self.symbol();
        self.depth -= 1;
        r
    }

    fn qualified(&self, unq: &Unq, quals: &[String]) -> String {
        let class = || quals.first().cloned().unwrap_or_default();
        let last = match unq {
            Unq::Name(n) => n.clone(),
            Unq::Ctor(a) => class() + a,
            Unq::Dtor(a) => format!("~{}{a}", class()),
            Unq::Cast(a) => format!("operator{a}"),
        };
        let mut parts: Vec<&str> = quals.iter().rev().map(String::as_str).collect();
        parts.push(&last);
        parts.join("::")
    }

    /// a whole decorated name
    fn symbol(&mut self) -> Option<String> {
        self.expect(b'?')?;
        if self.eat_str("?_R0") {
            let t = self.ty(At::Ret)?;
            self.expect(b'@')?;
            self.expect(b'8')?;
            return Some(self.render(&t, "") + " `RTTI Type Descriptor'");
        }
        if self.s[self.i..].starts_with(b"?_C@_") {
            self.i = self.s.len();
            return Some("`string'".into());
        }
        let template_alone = self.s[self.i..].starts_with(b"$");
        if template_alone {
            // (`?$name@args@`: a template's name alone)
            self.i -= 1;
        }
        let unq = self.unqualified(false)?;
        if template_alone && self.peek().is_none() {
            return Some(self.qualified(&unq, &[]));
        }
        let quals = self.qualifiers()?;
        let name = self.qualified(&unq, &quals);
        let c = self.next()?;
        match c {
            b'0'..=b'4' => {
                let t = self.ty(At::Plain)?;
                let t = self.storage(t)?;
                if self.flag(NAME_ONLY) {
                    return Some(name);
                }
                let pre = match c {
                    b'0'..=b'2' => format!("{}{}",
                        if self.flag(NO_ACCESS_SPECIFIERS) { "" } else { ACCESS[(c - b'0') as usize + 1] },
                        if self.flag(NO_MEMBER_TYPE) { "" } else { "static " }),
                    _ => String::new(),
                };
                Some(pre + &self.render(&t, &format!(" {name}")))
            }
            b'6' | b'7' => {
                let cv = self.cv_letter()?;
                let mut s = String::new();
                if cv != 0 {
                    s.push_str(cv_word(cv));
                    s.push(' ');
                }
                s.push_str(&name);
                while self.peek().is_some() && !self.eat(b'@') {
                    let (u, q) = self.full_name(true)?;
                    s.push_str(&format!("{{for `{}'}}", self.qualified(&u, &q)));
                }
                Some(if self.flag(NAME_ONLY) { name } else { s })
            }
            b'8' => Some(name),
            b'5' => {
                let n = if self.peek().is_some() { format!("{{{}}}", self.signed()?) } else { String::new() };
                Some(name + &n)
            }
            b'$' => self.thunk(&name),
            _ => {
                let (acc, kind, this) = member(c)?;
                let adjust = if kind == 3 { Some(self.signed()?) } else { None };
                let f = self.function_rest(this)?;
                if self.flag(NAME_ONLY) {
                    return Some(if matches!(unq, Unq::Cast(_)) {
                        format!("{name} {}", self.render(f.ret.as_ref()?, ""))
                    } else { name });
                }
                let mut pre = String::new();
                if adjust.is_some() {
                    pre.push_str("[thunk]:");
                }
                if !self.flag(NO_ACCESS_SPECIFIERS) {
                    pre.push_str(ACCESS[acc as usize]);
                }
                if !self.flag(NO_MEMBER_TYPE) {
                    pre.push_str(["", "static ", "virtual ", "virtual "][kind as usize]);
                }
                let suffix = adjust.map(|a| format!("`adjustor{{{a}}}' ")).unwrap_or_default();
                Some(pre + &self.function_text(&f, &unq, &name, &suffix))
            }
        }
    }
    /// `$0`..`$5` vtordisp, `$R0`..`$R5` vtordispex, `$B` vcall thunks
    fn thunk(&mut self, name: &str) -> Option<String> {
        let c = self.next()?;
        if c == b'B' {
            let n = self.signed()?;
            self.expect(b'A')?;
            let cc = self.cc()?;
            if self.flag(NAME_ONLY) {
                return Some(format!("{name}{{{n}}}'"));
            }
            return Some(format!("[thunk]:{}{}{name}{{{n},{{flat}}}}' }}'", if cc.is_empty() { "" } else { " " }, sep_after(&cc)));
        }
        let ex = c == b'R';
        let d = if ex { self.next()? } else { c };
        if !(b'0'..=b'5').contains(&d) {
            return None;
        }
        let acc = (d - b'0') / 2 + 1;
        let nums = if ex { vec![self.unsigned()?, self.unsigned()?, self.unsigned()?, self.unsigned()?] } else { vec![self.unsigned()?, self.unsigned()?] };
        let f = self.function_rest(true)?;
        if self.flag(NAME_ONLY) {
            return Some(name.into());
        }
        let mut pre = String::from("[thunk]:");
        if !self.flag(NO_ACCESS_SPECIFIERS) {
            pre.push_str(ACCESS[acc as usize]);
        }
        if !self.flag(NO_MEMBER_TYPE) {
            pre.push_str("virtual ");
        }
        let suffix = format!("`{}{{{}}}' ", if ex { "vtordispex" } else { "vtordisp" }, nums.join(","));
        Some(pre + &self.function_text(&f, &Unq::Name(String::new()), name, &suffix))
    }

    /// a function as its declaration reads
    fn function_text(&self, f: &Func, unq: &Unq, name: &str, suffix: &str) -> String {
        let mut name = name.to_string();
        let cast = matches!(unq, Unq::Cast(_));
        if cast {
            if let Some(r) = &f.ret {
                name = format!("{name} {}{}", self.render(r, ""), if f.ret_cv { " " } else { "" });
            }
        }
        let args = if self.flag(TYPE_ONLY) { String::new() } else { format!("({})", f.args) };
        let decl = format!("{}{name}{suffix}{args}{}", sep_after(&f.cc), f.this_q);
        match &f.ret {
            Some(r) if !cast && !self.flag(NO_FUNCTION_RETURNS) => self.render(r, &decl),
            _ => decl,
        }
    }

    /// a function's qualifiers of `this`, calling convention, return type and parameters
    fn function_rest(&mut self, this: bool) -> Option<Func> {
        let this_q = if this { self.this_quals()? } else { String::new() };
        let cc = self.cc()?;
        let (ret, ret_cv) = if self.eat(b'@') {
            (None, false)
        } else {
            let storage = self.peek() == Some(b'?');
            let t = self.ty(At::Ret)?;
            let cv = storage && matches!(&t, Ty::Base { cv, .. } if *cv != 0);
            (Some(Box::new(t)), cv)
        };
        let args = if self.peek().is_none() {
            // Windows still renders a function with a missing parameter encoding.
            " ?? ".into()
        } else {
            let args = self.params_list()?;
            self.throw_spec()?;
            args
        };
        Some(Func { cc, ret, ret_cv, args, this_q })
    }
    /// `E` `I` `F` then a cv letter: as a member function prints them after its parameters
    fn this_quals(&mut self) -> Option<String> {
        let (mut ptr64, mut restrict, mut unaligned) = (false, false, false);
        loop {
            match self.peek()? {
                b'E' => ptr64 = true,
                b'I' => restrict = true,
                b'F' => unaligned = true,
                _ => break,
            }
            self.i += 1;
        }
        let cv = self.cv_letter()?;
        let mut q = String::new();
        if !self.flag(NO_CV_THISTYPE) {
            q.push_str(cv_word(cv));
        }
        if !self.flag(NO_MS_THISTYPE) {
            for (on, k) in [(unaligned, "__unaligned"), (restrict, "__restrict")] {
                let k = self.kw(k);
                if on && !k.is_empty() {
                    if !q.is_empty() {
                        q.push(' ');
                    }
                    q.push_str(&k);
                }
            }
        }
        let p64 = if ptr64 && !self.flag(NO_MS_THISTYPE) { self.kw("__ptr64") } else { String::new() };
        if !q.is_empty() || !p64.is_empty() {
            q = format!("{q} {p64}");
        }
        Some(q)
    }
    fn cv_letter(&mut self) -> Option<u8> {
        Some(match self.next()? {
            b'A' => 0,
            b'B' => 1,
            b'C' => 2,
            b'D' => 3,
            _ => return None,
        })
    }
    fn cc(&mut self) -> Option<String> {
        let k = match self.next()? {
            b'A' | b'B' => "__cdecl",
            b'C' | b'D' => "__pascal",
            b'E' | b'F' => "__thiscall",
            b'G' | b'H' => "__stdcall",
            b'I' | b'J' => "__fastcall",
            b'K' | b'L' => "",
            b'M' | b'N' => "__clrcall",
            b'O' | b'P' => "__eabi",
            b'Q' => "__vectorcall",
            b'S' => "__swift_1",
            b'U' => "__swift_2",
            b'W' => "__swift_3",
            _ => return None,
        };
        Some(if self.flag(NO_ALLOCATION_LANGUAGE) || k.is_empty() { String::new() } else { self.kw(k) })
    }
    fn throw_spec(&mut self) -> Option<()> {
        if self.eat(b'Z') {
            return Some(());
        }
        if self.eat_str("_E") {
            return Some(());
        }
        // (a throw list: its types)
        let _ = self.params_list()?;
        Some(())
    }
    /// a parameter list: `X` (void), types ended by `@` or `Z` (`...`)
    fn params_list(&mut self) -> Option<String> {
        if self.eat(b'X') {
            return Some("void".into());
        }
        let mut out: Vec<String> = Vec::new();
        loop {
            match self.peek()? {
                b'@' => {
                    self.i += 1;
                    break;
                }
                b'Z' => {
                    self.i += 1;
                    out.push("...".into());
                    break;
                }
                c if c.is_ascii_digit() => {
                    self.i += 1;
                    let t = self.params.get((c - b'0') as usize)?.clone();
                    out.push(self.render(&t, ""));
                }
                _ => {
                    let at = self.i;
                    let t = self.ty(At::Arg)?;
                    if self.i - at > 1 && self.params.len() < 10 {
                        self.params.push(t.clone());
                    }
                    out.push(self.render(&t, ""));
                }
            }
            if out.iter().map(|a| a.len() + 1).sum::<usize>() > OUT_MAX {
                return None;
            }
        }
        Some(out.join(","))
    }

    /// a variable's storage class after its type: `E`, a cv letter (or a member pointer's,
    /// its class after it)
    fn storage(&mut self, t: Ty) -> Option<Ty> {
        let mut ptr64 = false;
        while self.eat(b'E') {
            ptr64 = true;
        }
        let cv = match self.next()? {
            c @ b'A'..=b'D' => c - b'A',
            c @ b'Q'..=b'T' => {
                self.qualified_name()?;
                c - b'Q'
            }
            _ => return None,
        };
        Some(match t {
            Ty::Ptr { mark, cv: own, ptr64: p, restrict, mut tail, ret, to } => {
                if cv != 0 {
                    tail.push(' ');
                    tail.push_str(cv_word(cv));
                }
                let k = self.kw("__ptr64");
                if ptr64 && !k.is_empty() {
                    tail.push(' ');
                    tail.push_str(&k);
                }
                // (the storage class's cv-qualifiers are the variable's own: they stand for the
                // pointer's)
                let _ = own;
                Ty::Ptr { mark, cv: 0, ptr64: p, restrict, tail, ret, to }
            }
            Ty::Base { name, cv: c, unaligned } => Ty::Base { name, cv: c | cv, unaligned },
            t => t,
        })
    }

    /// a class's name as a type names it (`Name@Outer@@`)
    fn qualified_name(&mut self) -> Option<String> {
        let (u, q) = self.full_name(true)?;
        Some(self.qualified(&u, &q))
    }

    /// a type (`ctx`: where it is, for what `X` and `?` mean there)
    fn ty(&mut self, ctx: At) -> Option<Ty> {
        self.depth += 1;
        if self.depth > 64 {
            return None;
        }
        let r = self.ty_inner(ctx);
        self.depth -= 1;
        r
    }
    fn ty_inner(&mut self, ctx: At) -> Option<Ty> {
        let base = |n: &str| Some(Ty::Base { name: n.into(), cv: 0, unaligned: false });
        let c = self.next()?;
        match c {
            b'C' => base("signed char"),
            b'D' => base("char"),
            b'E' => base("unsigned char"),
            b'F' => base("short"),
            b'G' => base("unsigned short"),
            b'H' => base("int"),
            b'I' => base("unsigned int"),
            b'J' => base("long"),
            b'K' => base("unsigned long"),
            b'M' => base("float"),
            b'N' => base("double"),
            b'O' => base("long double"),
            b'X' => base("void"),
            b'_' => base(match self.next()? {
                b'D' => "__int8",
                b'E' => "unsigned __int8",
                b'F' => "__int16",
                b'G' => "unsigned __int16",
                b'H' => "__int32",
                b'I' => "unsigned __int32",
                b'J' => "__int64",
                b'K' => "unsigned __int64",
                b'L' => "__int128",
                b'M' => "unsigned __int128",
                b'N' => "bool",
                b'Q' => "char8_t",
                b'S' => "char16_t",
                b'U' => "char32_t",
                b'W' => "wchar_t",
                _ => return None,
            }),
            b'T' | b'U' | b'V' => {
                let n = self.qualified_name()?;
                let k = match c {
                    b'T' => "union",
                    b'U' => "struct",
                    _ => "class",
                };
                base(&if self.flag(NAME_ONLY) { n } else { format!("{k} {n}") })
            }
            b'W' => {
                let k = self.next()?;
                if !(b'0'..=b'7').contains(&k) {
                    return None;
                }
                let n = self.qualified_name()?;
                base(&if self.flag(NAME_ONLY) { n } else { format!("enum {n}") })
            }
            b'Y' => self.array(0),
            b'A' | b'B' => self.pointer("&", if c == b'B' { 2 } else { 0 }, ctx),
            b'P' | b'Q' | b'R' | b'S' => self.pointer("*", c - b'P', ctx),
            b'?' => {
                // a storage class first (a return type, a template argument)
                let mut cv = self.cv_letter()?;
                let mut t = self.ty(At::Plain)?;
                if let Ty::Base { cv: c, .. } = &mut t {
                    *c |= std::mem::take(&mut cv);
                }
                Some(t)
            }
            b'$' => {
                if self.eat(b'$') {
                    match self.next()? {
                        b'C' => {
                            let cv = self.cv_letter()?;
                            let mut t = self.ty(ctx)?;
                            match &mut t {
                                Ty::Base { cv: c, .. } | Ty::Array { cv: c, .. } => *c |= cv,
                                Ty::Ptr { to, .. } => {
                                    if let Ty::Base { cv: c, .. } = to.as_mut() {
                                        *c |= cv;
                                    }
                                }
                                _ => {}
                            }
                            Some(t)
                        }
                        b'A' => {
                            let k = self.next()?;
                            let class = if k == b'8' { Some(self.qualified_name()?) } else if k == b'6' { None } else { return None };
                            self.function_type(class)
                        }
                        b'B' => {
                            self.expect(b'Y')?;
                            self.array(0)
                        }
                        b'Q' => self.pointer("&&", 0, ctx),
                        b'R' => self.pointer("&&", 2, ctx),
                        b'T' => base("std::nullptr_t"),
                        _ => None,
                    }
                } else {
                    None
                }
            }
            _ => None,
        }
    }
    fn array(&mut self, cv: u8) -> Option<Ty> {
        let (neg, n) = self.number()?;
        if neg || n == 0 || n > 64 {
            return None;
        }
        let mut dims = String::new();
        for _ in 0..n {
            dims.push_str(&format!("[{}]", self.signed()?));
        }
        let elem = self.ty(At::Plain)?;
        Some(Ty::Array { cv, dims, elem: Box::new(elem) })
    }
    /// a function type (`P6`, `$$A6`) or member function type (`P8 Class@@`)
    fn function_type(&mut self, class: Option<String>) -> Option<Ty> {
        let f = self.function_rest(class.is_some())?;
        Some(Ty::Fn { cc: f.cc, ret: f.ret, args: f.args, this_q: f.this_q, class })
    }
    /// a pointer or reference after its letter (`own`: its own cv-qualifiers)
    fn pointer(&mut self, mark: &str, own: u8, ctx: At) -> Option<Ty> {
        let mut mark = mark.to_string();
        let (mut ptr64, mut restrict, mut unaligned) = (false, false, false);
        loop {
            if self.eat_str("$A") {
                // (C++/CLI: a handle, a tracking reference)
                mark = if mark == "*" { "^".into() } else { "%".into() };
                continue;
            }
            match self.peek()? {
                b'E' => ptr64 = true,
                b'I' => restrict = true,
                b'F' => unaligned = true,
                _ => break,
            }
            self.i += 1;
        }
        let k = self.peek()?;
        let to = if (b'6'..=b'9').contains(&k) {
            self.i += 1;
            let class = if k >= b'8' { Some(self.qualified_name()?) } else { None };
            self.function_type(class)?
        } else {
            let (cv, member) = match self.next()? {
                c @ b'A'..=b'D' => (c - b'A', None),
                c @ b'Q'..=b'T' => (c - b'Q', Some(self.qualified_name()?)),
                _ => return None,
            };
            if let Some(m) = member {
                mark = format!("{m}::{mark}");
            }
            let mut t = if self.eat(b'Y') { self.array(cv)? } else { self.ty(At::Plain)? };
            match &mut t {
                Ty::Base { cv: c, unaligned: u, .. } => {
                    *c |= cv;
                    *u |= unaligned;
                }
                // (a pointer to a const pointer: the cv letter is the inner one's own)
                Ty::Ptr { cv: c, .. } => *c |= cv,
                _ => {}
            }
            t
        };
        Some(Ty::Ptr { mark, cv: own, ptr64, restrict, tail: String::new(), ret: matches!(ctx, At::Ret), to: Box::new(to) })
    }

    /// a type alone (type_info::name's, TYPE_ONLY): `?` and a storage letter, or a type
    fn datatype(&mut self) -> Option<String> {
        let t = if self.eat(b'?') {
            let cv = self.cv_letter()?;
            let t = self.ty(At::Plain)?;
            match t {
                Ty::Base { name, cv: c, unaligned } => Ty::Base { name, cv: c | cv, unaligned },
                t => t,
            }
        } else {
            self.ty(At::Plain)?
        };
        Some(self.render(&t, ""))
    }

    /// a type written out around the declarator `decl` (what it qualifies: a name, `*`, ...)
    fn render(&self, t: &Ty, decl: &str) -> String {
        match t {
            Ty::Base { name, cv, unaligned } => {
                let mut s = name.clone();
                if *cv != 0 {
                    s.push(' ');
                    s.push_str(cv_word(*cv));
                }
                let k = self.kw("__unaligned");
                if *unaligned && !k.is_empty() {
                    s.push(' ');
                    s.push_str(&k);
                }
                join(s, decl)
            }
            Ty::Ptr { mark, cv, ptr64, restrict, tail, ret, to } => match to.as_ref() {
                Ty::Fn { cc, class, .. } => {
                    // (a pointer to function returned: Windows prints no cv-qualifiers of it)
                    let own = if *ret { "" } else { cv_word(*cv) };
                    let cls = class.as_ref().map(|c| format!(" {c}::")).unwrap_or_default();
                    let inner = format!("({cc}{cls}{mark}{own}{tail}{decl})");
                    self.render_fn(to, &inner)
                }
                Ty::Array { cv: acv, dims, elem } => {
                    let pre = if *acv != 0 { format!("{} ", cv_word(*acv)) } else { String::new() };
                    let inner = format!("({pre}{}{})", self.ptr_mods(mark, *cv, *ptr64, *restrict, tail), sep(decl));
                    self.render(elem, &format!("{inner}{dims}"))
                }
                to => self.render(to, &format!("{}{}", self.ptr_mods(mark, *cv, *ptr64, *restrict, tail), sep(decl))),
            },
            Ty::Fn { cc, .. } => self.render_fn(t, &format!("{}{}", sep_after(cc), decl.trim_start())),
            Ty::Array { dims, elem, .. } => {
                let d = if decl.is_empty() { dims.clone() } else { format!("{}{dims}", decl) };
                self.render(elem, &d)
            }
        }
    }
    fn ptr_mods(&self, mark: &str, cv: u8, ptr64: bool, restrict: bool, tail: &str) -> String {
        let mut s = mark.to_string();
        for (on, k) in [(ptr64, "__ptr64"), (restrict, "__restrict")] {
            let k = self.kw(k);
            if on && !k.is_empty() {
                s.push(' ');
                s.push_str(&k);
            }
        }
        if cv != 0 {
            s.push(' ');
            s.push_str(cv_word(cv));
        }
        s + tail
    }
    fn render_fn(&self, f: &Ty, inner: &str) -> String {
        let Ty::Fn { ret, args, this_q, .. } = f else { return String::new() };
        let decl = format!("{inner}({args}){this_q}");
        match ret {
            Some(r) => self.render(r, &decl),
            None => decl,
        }
    }
}

/// a keyword then a space (nothing when there is no keyword)
fn sep_after(k: &str) -> String {
    if k.is_empty() {
        String::new()
    } else {
        format!("{k} ")
    }
}

#[derive(Clone, Copy)]
enum At {
    Plain,
    Arg,
    Ret,
}

struct Func {
    cc: String,
    ret: Option<Box<Ty>>,
    /// the return type has a storage class with cv-qualifiers (`?B`)
    ret_cv: bool,
    args: String,
    this_q: String,
}

// ---------------------------------------------------------------------------------------------
// the exports: msvcrt's __unDName / __unDNameEx, type_info's members, the UCRT's
// __std_type_info_name, dbghelp's (imagehlp's) UnDecorateSymbolName

use crate::api::{implement, Ctx};
use crate::mem::{self, cstr, ps, rp, wp};

/// `mangled` undecorated, or itself when it is not a name undname knows (Windows' answer)
pub fn undecorate(mangled: &[u8], flags: u32) -> Vec<u8> {
    if mangled.is_empty() {
        return b" ?? ".to_vec();
    }
    let s = String::from_utf8_lossy(mangled);
    match undname(&s, flags) {
        Some(d) => d.into_bytes(),
        None => mangled.to_vec(),
    }
}

/// __unDName(buffer, mangled, length, alloc, free, flags): into `buffer` (cut to `length`), or
/// a block of the program's `alloc` when it is NULL; NULL without a name or an allocator
fn __unDName(c: &mut Ctx) -> u32 {
    let (buf, name, n, alloc, flags) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(5));
    undname_into(c, buf, name, n, alloc, flags)
}
/// __unDNameEx(buffer, mangled, length, alloc, free, get_parameter, flags)
fn __unDNameEx(c: &mut Ctx) -> u32 {
    let (buf, name, n, alloc, flags) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(6));
    undname_into(c, buf, name, n, alloc, flags)
}
fn undname_into(c: &mut Ctx, buf: u32, name: u32, n: u32, alloc: u32, flags: u32) -> u32 {
    if name == 0 || alloc == 0 {
        return 0;
    }
    let out = undecorate(&cstr(name), flags);
    let (dst, cap) = if buf == 0 {
        let d = c.call(alloc, &[out.len() as u32 + 1]);
        (d, out.len() as u32 + 1)
    } else {
        (buf, n)
    };
    if dst == 0 || cap == 0 {
        return 0;
    }
    let k = out.len().min(cap as usize - 1);
    mem::write(dst, &out[..k]);
    mem::w8(dst + k as u32, 0);
    dst
}

/// UnDecorateSymbolName(name, out, size, flags): the length written
fn symbol_output_len(len: usize, cap: u32) -> usize {
    // DbgHelp reserves a spare character as well as the terminator (winref's 8-byte
    // buffer contains "void _", six characters; __unDName uses cap - 1 instead).
    len.min(cap.saturating_sub(2) as usize)
}
fn UnDecorateSymbolName(c: &mut Ctx) -> u32 {
    let (name, out, n, flags) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if name == 0 || out == 0 || n == 0 {
        crate::k32::last_error(87);
        return 0;
    }
    let d = undecorate(&cstr(name), flags);
    let k = symbol_output_len(d.len(), n);
    mem::write(out, &d[..k]);
    mem::w8(out + k as u32, 0);
    k as u32
}
fn UnDecorateSymbolNameW(c: &mut Ctx) -> u32 {
    let (name, out, n, flags) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if name == 0 || out == 0 || n == 0 {
        crate::k32::last_error(87);
        return 0;
    }
    let s = String::from_utf16_lossy(&mem::wstr(name));
    let d: Vec<u16> = undname(&s, flags).unwrap_or(s).encode_utf16().collect();
    let k = d.len().min(n as usize - 1);
    for (i, ch) in d[..k].iter().enumerate() {
        mem::w16(out + 2 * i as u32, *ch);
    }
    mem::w16(out + 2 * k as u32, 0);
    k as u32
}

// type_info { vtable, undecorated name (made on first ask), decorated name[] } (x86:
// thiscall, `this` in ECX)

fn this(c: &Ctx) -> u32 {
    if mem::x64() { c.a(0) } else { c.cpu.regs[1] }
}
/// argument `i` after `this`
fn arg(c: &Ctx, i: u32) -> u32 {
    if mem::x64() { c.a(i + 1) } else { c.a(i) }
}
fn decorated(t: u32) -> Vec<u8> {
    cstr(t + 2 * ps())
}
/// the undecorated name of a type descriptor's decorated one (past its '.'), cached at `slot`
fn type_name(slot: u32, dec: &[u8]) -> u32 {
    let have = rp(slot);
    if have != 0 {
        return have;
    }
    let d = undecorate(dec.strip_prefix(b".").unwrap_or(dec), TYPE_ONLY | 0x800);
    let p = mem::heap_alloc(d.len() as u32 + 1, true);
    mem::write(p, &d);
    wp(slot, p);
    p
}
fn type_info_name(c: &mut Ctx) -> u32 {
    let t = this(c);
    type_name(t + ps(), &decorated(t))
}
fn type_info_raw_name(c: &mut Ctx) -> u32 {
    this(c) + 2 * ps()
}
/// before, ==, != compare the decorated names past their first character
fn cmp(c: &Ctx) -> std::cmp::Ordering {
    cmp_names(this(c) + 2 * ps(), arg(c, 0) + 2 * ps())
}
/// Two decorated names compared past their first character, in guest memory (no copies: C++
/// code compares typeids in loops), as their byte strings compare
pub fn cmp_names(a: u32, b: u32) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    if a == b {
        return Equal;
    }
    let (sa, sb) = (if mem::r8(a) == 0 { None } else { Some(a + 1) }, if mem::r8(b) == 0 { None } else { Some(b + 1) });
    let byte = |s: Option<u32>, i: u32| s.map_or(0, |s| mem::r8(s.wrapping_add(i)));
    for i in 0..1 << 20 {
        let (x, y) = (byte(sa, i), byte(sb, i));
        match (x, y) {
            (0, 0) => return Equal,
            (0, _) => return Less,
            (_, 0) => return Greater,
            _ if x != y => return x.cmp(&y),
            _ => {}
        }
    }
    Equal
}
fn type_info_before(c: &mut Ctx) -> u32 {
    cmp(c).is_lt() as u32
}
fn type_info_eq(c: &mut Ctx) -> u32 {
    cmp(c).is_eq() as u32
}
fn type_info_ne(c: &mut Ctx) -> u32 {
    cmp(c).is_ne() as u32
}
fn type_info_dtor(c: &mut Ctx) -> u32 {
    let t = this(c);
    let p = rp(t + ps());
    if p != 0 {
        mem::heap_free(p);
        wp(t + ps(), 0);
    }
    0
}
/// __std_type_info_name(data, list): data is { undecorated name, decorated name[] }
fn __std_type_info_name(c: &mut Ctx) -> u32 {
    let d = c.a(0);
    type_name(d, &cstr(d + ps()))
}

pub fn init() {
    for dll in ["msvcrt", "ucrtbase", "msvcr70", "msvcr71", "msvcr80", "msvcr90", "msvcr100", "msvcr110", "msvcr120"] {
        implement(dll, "__unDName", __unDName);
        implement(dll, "__unDNameEx", __unDNameEx);
    }
    for dll in ["ucrtbase", "vcruntime140"] {
        implement(dll, "__std_type_info_name", __std_type_info_name);
    }
    for dll in ["dbghelp", "imagehlp"] {
        implement(dll, "UnDecorateSymbolName", UnDecorateSymbolName);
        implement(dll, "UnDecorateSymbolNameW", UnDecorateSymbolNameW);
    }
    let members: [(&str, &str, crate::api::Handler); 6] = [
        ("?name@type_info@@QBEPBDXZ", "?name@type_info@@QEBAPEBDXZ", type_info_name),
        ("?raw_name@type_info@@QBEPBDXZ", "?raw_name@type_info@@QEBAPEBDXZ", type_info_raw_name),
        ("?before@type_info@@QBEHABV1@@Z", "?before@type_info@@QEBAHAEBV1@@Z", type_info_before),
        ("??8type_info@@QBEHABV0@@Z", "??8type_info@@QEBAHAEBV0@@Z", type_info_eq),
        ("??9type_info@@QBEHABV0@@Z", "??9type_info@@QEBAHAEBV0@@Z", type_info_ne),
        ("??1type_info@@UAE@XZ", "??1type_info@@UEAA@XZ", type_info_dtor),
    ];
    for dll in ["msvcrt", "msvcr70", "msvcr71", "msvcr80", "msvcr90", "msvcr100", "msvcr110", "msvcr120"] {
        for (x86, x64, f) in members {
            implement(dll, x86, f);
            implement(dll, x64, f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(m: &str) -> String {
        undname(m, 0).unwrap_or_else(|| m.to_string())
    }

    #[test]
    fn functions() {
        assert_eq!(u("?f@@YAXXZ"), "void __cdecl f(void)");
        assert_eq!(u("?g@ns@@YGHHPBD@Z"), "int __stdcall ns::g(int,char const *)");
        assert_eq!(u("?m@C@@QAEXH@Z"), "public: void __thiscall C::m(int)");
        assert_eq!(u("?m@C@@QBEXXZ"), "public: void __thiscall C::m(void)const ");
        assert_eq!(u("?m@C@@UEAAXXZ"), "public: virtual void __cdecl C::m(void) __ptr64");
        assert_eq!(u("?m@C@@SAHXZ"), "public: static int __cdecl C::m(void)");
        assert_eq!(u("?v@@YAXHZZ"), "void __cdecl v(int,...)");
        assert_eq!(u("?f@@YIX_N_W_J@Z"), "void __fastcall f(bool,wchar_t,__int64)");
    }

    #[test]
    fn special_members() {
        assert_eq!(u("??0Foo@@QAE@XZ"), "public: __thiscall Foo::Foo(void)");
        assert_eq!(u("??1Foo@@UAE@XZ"), "public: virtual __thiscall Foo::~Foo(void)");
        assert_eq!(u("??4Foo@@QAEAAV0@ABV0@@Z"), "public: class Foo & __thiscall Foo::operator=(class Foo const &)");
        assert_eq!(u("??BFoo@@QBEHXZ"), "public: __thiscall Foo::operator int(void)const ");
        assert_eq!(u("??_GFoo@@UAEPAXI@Z"), "public: virtual void * __thiscall Foo::`scalar deleting destructor'(unsigned int)");
        assert_eq!(u("??_7Foo@@6B@"), "const Foo::`vftable'");
        assert_eq!(u("??_R4Foo@@6B@"), "const Foo::`RTTI Complete Object Locator'");
        assert_eq!(u("??_R0?AVFoo@@@8"), "class Foo `RTTI Type Descriptor'");
        assert_eq!(u("??_R1A@?0A@EA@Foo@@8"), "Foo::`RTTI Base Class Descriptor at (0,-1,0,64)'");
        assert_eq!(u("??_C@_03KOEHGMDN@abc?$AA@"), "`string'");
    }

    #[test]
    fn back_references() {
        // names: Bar(0) Foo(1); parameter types: `PAVBar@Foo@@` (0)
        assert_eq!(u("?f@Bar@Foo@@QAEXPAV12@0@Z"), "public: void __thiscall Foo::Bar::f(class Foo::Bar *,class Foo::Bar *)");
        // a one-letter type is not remembered: `0` is the first longer one
        assert_eq!(u("?f@@YAXHPAH0@Z"), "void __cdecl f(int,int *,int *)");
    }

    #[test]
    fn templates() {
        assert_eq!(u("?f@?$Vec@H$02@@QAEXXZ"), "public: void __thiscall Vec<int,3>::f(void)");
        // `>>` is written `> >`; an argument's own names start a table of their own
        assert_eq!(u("?x@@3V?$A@V?$B@H@@@@A"), "class A<class B<int> > x");
        assert_eq!(u("??$max@N@@YANNN@Z"), "double __cdecl max<double>(double,double)");
        assert_eq!(u("?$T@$0?5@"), "T<-6>");
    }

    #[test]
    fn data_and_pointers() {
        assert_eq!(u("?n@@3HA"), "int n");
        assert_eq!(u("?s@C@@2PBDB"), "public: static char const * const C::s");
        assert_eq!(u("?p@@3PAY01HA"), "int (* p)[2]");
        assert_eq!(u("?fp@@3P6AHH@ZA"), "int (__cdecl* fp)(int)");
        assert_eq!(u("?fpp@@3PAP6AHH@ZA"), "int (__cdecl** fpp)(int)");
        assert_eq!(u("?pm@@3PQC@@HQ1@"), "int C::* pm");
        assert_eq!(u("?f@@YAXQAH@Z"), "void __cdecl f(int * const)");
        assert_eq!(u("?f@@YAXAEBV?$basic_string@D@@@Z"), "void __cdecl f(class basic_string<char> const & __ptr64)");
    }

    #[test]
    fn flags() {
        let m = "?m@C@@UEBAXXZ";
        assert_eq!(undname(m, NAME_ONLY).unwrap(), "C::m");
        assert_eq!(undname(m, NO_ACCESS_SPECIFIERS).unwrap(), "virtual void __cdecl C::m(void)const __ptr64");
        assert_eq!(undname(m, NO_MS_KEYWORDS).unwrap(), "public: virtual void C::m(void)const ");
        assert_eq!(undname(m, NO_LEADING_UNDERSCORES).unwrap(), "public: virtual void cdecl C::m(void)const ptr64");
        assert_eq!(undname(m, NO_MS_THISTYPE | NO_CV_THISTYPE).unwrap(), "public: virtual void __cdecl C::m(void)");
        assert_eq!(undname("?AVFoo@ns@@", TYPE_ONLY).unwrap(), "class ns::Foo");
        assert_eq!(undname("PBD", TYPE_ONLY).unwrap(), "char const *");
    }

    /// review b09: template arguments naming templates naming templates, 200000 levels deep,
    /// are refused (no host stack overflow), and an argument list past OUT_MAX is refused while
    /// a long one below it is undecorated
    #[test]
    fn deep_and_long_names_are_refused() {
        let deep = format!("?x@@3V{}@A", "?$A@$1?".repeat(200_000));
        let t = std::thread::Builder::new().stack_size(256 << 10).spawn(move || undname(&deep, 0)).unwrap();
        assert_eq!(t.join().unwrap(), None);
        let long = format!("?x@@3V?$A@{}@@A", "H".repeat(20_000));
        assert_eq!(undname(&long, 0), None);
        let fine = format!("?x@@3V?$A@{}@@A", "H".repeat(1000));
        let out = undname(&fine, 0).expect("1000 arguments");
        assert!(out.contains(&"int,".repeat(999)));
    }

    #[test]
    fn rejects() {
        for m in ["", "?", "?f@@", "f", "?f@@YAXXZjunk", "?f.g@@YAXXZ", "?f@@3H", "?0@@YAXXZ"] {
            assert_eq!(undname(m, 0), None, "{m}");
        }
    }

    #[test]
    fn recorded_windows_symbol_names() {
        let answers = include_str!("../../../tests/winref/golden/undname-32/stdout.txt");
        for line in answers.lines() {
            let Some((query, answer)) = line.split_once(": ") else { continue };
            let Some((name, flags)) = query.rsplit_once(' ') else { continue };
            if name.contains(' ') { continue; } // the separate __unDName buffer probes
            let flags = if let Some(hex) = flags.strip_prefix("0x") {
                u32::from_str_radix(hex, 16).unwrap()
            } else {
                let Ok(flags) = flags.parse::<u32>() else { continue };
                flags
            };
            let Some((_, answer)) = answer.split_once(" [") else { continue };
            let answer = answer.strip_suffix(']').unwrap();
            assert_eq!(String::from_utf8(undecorate(name.as_bytes(), flags)).unwrap(), answer, "{query}");
        }
        let d = undecorate(b"?f@@YAXXZ", 0);
        let n = symbol_output_len(d.len(), 8);
        assert_eq!(&d[..n], b"void _");
        assert_eq!(symbol_output_len(d.len(), 1), 0);
        assert_eq!(symbol_output_len(d.len(), 2), 0);
    }

    /// UNDNAME_VECTORS=file: lines of `id, mangled, flags, wanted[, also accepted]` (tabs),
    /// as an outside suite (Wine's msvcrt tests) has them; prints every miss
    #[test]
    fn outside_vectors() {
        let Ok(path) = std::env::var("UNDNAME_VECTORS") else { return };
        let text = std::fs::read_to_string(path).unwrap();
        let mut misses = 0;
        for l in text.lines() {
            let v: Vec<&str> = l.split('\t').collect();
            let flags = v[2].parse().unwrap();
            let got = undname(v[1], flags).unwrap_or_else(|| v[1].to_string());
            if got != v[3] && v.get(4).is_none_or(|b| b.is_empty() || got != *b) {
                misses += 1;
                eprintln!("{}: {}\n   got  {got}\n   want {}", v[0], v[1], v[3]);
            }
        }
        assert_eq!(misses, 0);
    }

    use proptest::prelude::*;

    /// a small mangler: (decorated, undecorated) types, without back-references
    fn ty_strategy() -> impl Strategy<Value = (String, String)> {
        let leaf = prop_oneof![
            Just(("H".to_string(), "int".to_string())),
            Just(("D".to_string(), "char".to_string())),
            Just(("_N".to_string(), "bool".to_string())),
            Just(("N".to_string(), "double".to_string())),
            "[a-z][a-z0-9_]{0,6}".prop_map(|n| (format!("U{n}@@"), format!("struct {n}"))),
        ];
        // (a pointer's own const is its letter outermost, its pointee's cv letter inside)
        let ptrs = leaf.prop_recursive(4, 16, 2, |inner| {
            prop_oneof![
                inner.clone().prop_map(|(m, d)| (format!("PA{m}"), format!("{d} *"))),
                inner.prop_map(|(m, d)| (format!("PB{m}"), format!("{d} const *"))),
            ]
        });
        prop_oneof![
            ptrs.clone(),
            ptrs.clone().prop_map(|(m, d)| (format!("AA{m}"), format!("{d} &"))),
            ptrs.prop_map(|(m, d)| (format!("QA{m}"), format!("{d} * const"))),
        ]
    }

    proptest! {
        #[test]
        fn mangled_parameters_undecorate(name in "[a-z][a-z0-9_]{0,8}", t in ty_strategy()) {
            // one parameter: no back-references to worry about
            let m = format!("?{name}@@YAX{}@Z", t.0);
            prop_assert_eq!(undname(&m, 0), Some(format!("void __cdecl {name}({})", t.1)));
            // (a const pointer variable has the const in its storage class too)
            let m = format!("?{name}@@3{}{}", t.0, if t.0.starts_with('Q') { "B" } else { "A" });
            prop_assert_eq!(undname(&m, 0), Some(join(t.1.clone(), &format!(" {name}"))));
        }

        #[test]
        fn never_panics(s in "[?@$A-Z0-9_a-z]{0,40}", flags in 0u32..0x4000) {
            let _ = undname(&s, flags);
        }

        #[test]
        /// review b09: type_info comparisons in guest memory order names as their copies did
        #[test]
        fn names_compare_in_place_as_copies(a in proptest::collection::vec(1u8..=255, 0..12), b in proptest::collection::vec(1u8..=255, 0..12), same in any::<bool>()) {
            let _g = crate::test_guest::enter();
            let b = if same { a.clone() } else { b };
            let put = |v: &[u8]| {
                let p = mem::heap_alloc(v.len() as u32 + 1, true);
                mem::write(p, v);
                mem::w8(p + v.len() as u32, 0);
                p
            };
            let (pa, pb) = (put(&a), put(&b));
            let want = a.get(1..).unwrap_or(&[]).cmp(b.get(1..).unwrap_or(&[]));
            prop_assert_eq!(cmp_names(pa, pb), want);
            prop_assert_eq!(cmp_names(pb, pa), want.reverse());
            mem::heap_free(pa);
            mem::heap_free(pb);
        }

        fn never_panics_near_names(base in prop::sample::select(vec![
            "??0?$basic_filebuf@DU?$char_traits@D@std@@@std@@QAE@ABV01@@Z",
            "?Qux@Bar@@0PAPAP6AHPAV1@AAH1PAH@ZA",
            "??_R1A@?0A@EA@Foo@@8",
            "?f@T@@QAEHQCY1BE@BO@D@Z",
        ]), cut in 0usize..80, ch in "[?@$0-9A-Z]") {
            let mut s = base.to_string();
            let k = cut.min(s.len());
            s.insert_str(k, &ch);
            let _ = undname(&s, 0);
            let _ = undname(&base[..k], 0);
        }
    }
}
