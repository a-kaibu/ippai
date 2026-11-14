//! Conditional expressions (MsiEvaluateCondition, the Condition columns of the sequence,
//! Feature, Component and Control tables).
//!
//! expr: IMP < EQV < XOR < OR < AND < NOT < comparison; values: properties, %ENV,
//! $component / ?component (action / installed state), &feature / !feature, integers,
//! "strings"; comparisons = <> < > <= >=, >< (contains), << (begins), >> (ends), each with a
//! leading ~ for case-insensitive.

pub trait Env {
    fn property(&self, name: &str) -> String;
    fn env(&self, name: &str) -> String {
        std::env::var(name).unwrap_or_default()
    }
    /// (installed, action) of a component; None: no such component
    fn component(&self, _name: &str) -> Option<(i32, i32)> {
        None
    }
    fn feature(&self, _name: &str) -> Option<(i32, i32)> {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cond {
    False = 0,
    True = 1,
    /// the condition was empty
    None = 2,
    Error = 3,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Sym(String),
    Str(String),
    Int(i64),
    Op(String),
    LP,
    RP,
}

fn lex(s: &str) -> Option<Vec<Tok>> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '(' {
            out.push(Tok::LP);
            i += 1;
        } else if ch == ')' {
            out.push(Tok::RP);
            i += 1;
        } else if ch == '"' {
            let st = i + 1;
            i = st;
            while i < c.len() && c[i] != '"' {
                i += 1;
            }
            if i >= c.len() {
                return None;
            }
            out.push(Tok::Str(c[st..i].iter().collect()));
            i += 1;
        } else if ch.is_ascii_digit() || (ch == '-' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let st = i;
            i += 1;
            while i < c.len() && c[i].is_ascii_digit() {
                i += 1;
            }
            let t: String = c[st..i].iter().collect();
            out.push(Tok::Int(t.parse().ok()?));
        } else if "=<>~".contains(ch) {
            let st = i;
            if ch == '~' {
                i += 1;
            }
            let two: String = c[i..(i + 2).min(c.len())].iter().collect();
            let len = match two.as_str() {
                "<>" | "<=" | ">=" | "><" | "<<" | ">>" => 2,
                _ if i < c.len() && "=<>".contains(c[i]) => 1,
                _ => return None,
            };
            i += len;
            out.push(Tok::Op(c[st..i].iter().collect()));
        } else if ch.is_alphanumeric() || "_%$?&!.#".contains(ch) {
            let st = i;
            i += 1;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '.') {
                i += 1;
            }
            out.push(Tok::Sym(c[st..i].iter().collect()));
        } else {
            return None;
        }
    }
    Some(out)
}

#[derive(Clone, Debug, PartialEq)]
enum V {
    Int(i32),
    Str(String),
}

struct P<'a> {
    t: Vec<Tok>,
    i: usize,
    env: &'a dyn Env,
    /// NOTs and parentheses open around the current token
    depth: usize,
}

/// How deeply NOT and parentheses may nest: past it the condition is an error (the parser
/// recurses once per level, and a guest's condition must not overflow the host stack).
pub const MAX_DEPTH: usize = 200;

fn word(t: Option<&Tok>, w: &str) -> bool {
    matches!(t, Some(Tok::Sym(s)) if s.eq_ignore_ascii_case(w))
}

const WORDS: &[&str] = &["NOT", "AND", "OR", "XOR", "EQV", "IMP"];

impl P<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn take_word(&mut self, w: &str) -> bool {
        if word(self.peek(), w) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn imp(&mut self) -> Option<bool> {
        let mut a = self.eqv()?;
        while self.take_word("IMP") {
            let b = self.eqv()?;
            a = !a || b;
        }
        Some(a)
    }
    fn eqv(&mut self) -> Option<bool> {
        let mut a = self.xor()?;
        while self.take_word("EQV") {
            let b = self.xor()?;
            a = a == b;
        }
        Some(a)
    }
    fn xor(&mut self) -> Option<bool> {
        let mut a = self.or()?;
        while self.take_word("XOR") {
            let b = self.or()?;
            a ^= b;
        }
        Some(a)
    }
    fn or(&mut self) -> Option<bool> {
        let mut a = self.and()?;
        while self.take_word("OR") {
            let b = self.and()?;
            a = a || b;
        }
        Some(a)
    }
    fn and(&mut self) -> Option<bool> {
        let mut a = self.not()?;
        while self.take_word("AND") {
            let b = self.not()?;
            a = a && b;
        }
        Some(a)
    }
    fn deeper(&mut self) -> Option<()> {
        self.depth += 1;
        (self.depth <= MAX_DEPTH).then_some(())
    }
    fn not(&mut self) -> Option<bool> {
        if self.take_word("NOT") {
            self.deeper()?;
            let v = !self.not()?;
            self.depth -= 1;
            return Some(v);
        }
        self.comparison()
    }
    fn comparison(&mut self) -> Option<bool> {
        if self.peek() == Some(&Tok::LP) {
            self.i += 1;
            self.deeper()?;
            let v = self.imp()?;
            self.depth -= 1;
            if self.peek() != Some(&Tok::RP) {
                return None;
            }
            self.i += 1;
            return Some(v);
        }
        let a = self.value()?;
        if let Some(Tok::Op(op)) = self.peek().cloned() {
            self.i += 1;
            let b = self.value()?;
            return Some(compare(&a, &op, &b));
        }
        Some(match a {
            (V::Int(i), _) => i != 0,
            (V::Str(s), _) => !s.is_empty(),
        })
    }
    /// A value and whether it came from a literal.
    fn value(&mut self) -> Option<(V, bool)> {
        match self.peek().cloned()? {
            Tok::Int(n) => {
                self.i += 1;
                Some((V::Int(i32::try_from(n).ok()?), true))
            }
            Tok::Str(s) => {
                self.i += 1;
                Some((V::Str(s), true))
            }
            Tok::Sym(s) => {
                if WORDS.iter().any(|w| w.eq_ignore_ascii_case(&s)) {
                    return None;
                }
                self.i += 1;
                let (sigil, name) = s.split_at(s.chars().next()?.len_utf8());
                Some(match sigil {
                    "%" => (V::Str(self.env.env(name)), false),
                    "$" | "?" => match self.env.component(name) {
                        Some((inst, act)) => (V::Int(if sigil == "$" { act } else { inst }), false),
                        None => (V::Str(String::new()), false),
                    },
                    "&" | "!" => match self.env.feature(name) {
                        Some((inst, act)) => (V::Int(if sigil == "&" { act } else { inst }), false),
                        None => (V::Str(String::new()), false),
                    },
                    _ => (V::Str(self.env.property(&s)), false),
                })
            }
            _ => None,
        }
    }
}

fn int_of(s: &str) -> Option<i32> {
    // (no spaces around it: "0 " is a string)
    crate::record::parse_int(s)
}

fn compare(a: &(V, bool), op: &str, b: &(V, bool)) -> bool {
    let (nocase, op) = match op.strip_prefix('~') {
        Some(o) => (true, o),
        None => (false, op),
    };
    // (a value that is an integer counts as one, a quoted literal never does)
    let ints = |x: &(V, bool)| match &x.0 {
        V::Int(i) => Some(*i),
        V::Str(_) if x.1 => None,
        V::Str(s) => int_of(s),
    };
    // an integer on either side: compare as integers when the other side is one too
    let any_int = matches!(a.0, V::Int(_)) || matches!(b.0, V::Int(_));
    if any_int || (ints(a).is_some() && ints(b).is_some()) {
        match (ints(a), ints(b)) {
            (Some(x), Some(y)) => {
                return match op {
                    "=" => x == y,
                    "<>" => x != y,
                    "<" => x < y,
                    ">" => x > y,
                    "<=" => x <= y,
                    ">=" => x >= y,
                    "><" => x & y != 0,
                    "<<" => (x >> 16) == y,
                    ">>" => (x & 0xffff) == y,
                    _ => false,
                };
            }
            _ if any_int => return op == "<>",
            _ => {}
        }
    }
    let s = |x: &V| match x {
        V::Int(i) => i.to_string(),
        V::Str(s) => s.clone(),
    };
    let (mut x, mut y) = (s(&a.0), s(&b.0));
    if nocase {
        x = x.to_uppercase();
        y = y.to_uppercase();
    }
    match op {
        "=" => x == y,
        "<>" => x != y,
        "<" => x < y,
        ">" => x > y,
        "<=" => x <= y,
        ">=" => x >= y,
        // (nothing is in an empty string, not even an empty one)
        "><" => !x.is_empty() && x.contains(&y),
        "<<" => !x.is_empty() && x.starts_with(&y),
        ">>" => !x.is_empty() && x.ends_with(&y),
        _ => false,
    }
}

pub fn evaluate(cond: &str, env: &dyn Env) -> Cond {
    if cond.trim().is_empty() {
        return Cond::None;
    }
    let Some(t) = lex(cond) else { return Cond::Error };
    let mut p = P { t, i: 0, env, depth: 0 };
    match p.imp() {
        Some(v) if p.i == p.t.len() => {
            if v {
                Cond::True
            } else {
                Cond::False
            }
        }
        _ => Cond::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct E;
    impl Env for E {
        fn property(&self, n: &str) -> String {
            match n {
                "VersionNT" => "601".into(),
                "Installed" => String::new(),
                "ProductName" => "Game".into(),
                "ONE" => "1".into(),
                "SP" => "0 ".into(),
                "N1234" => "1234".into(),
                "N1" => "1".into(),
                "HI" => "hi".into(),
                _ => String::new(),
            }
        }
        fn component(&self, n: &str) -> Option<(i32, i32)> {
            (n == "C1").then_some((-1, 3))
        }
    }

    #[test]
    fn mixed_types() {
        // (Windows' answers, from Wine's package tests)
        let t = |s: &str| evaluate(s, &E);
        assert_eq!(t(" 1 ~= \"1\" "), Cond::False);
        assert_eq!(t(" 1 = \"1\" "), Cond::False);
        assert_eq!(t(" 0 < \"100\" "), Cond::False);
        assert_eq!(t(" 100 > \"0\" "), Cond::False);
        assert_eq!(t("ONE = 1"), Cond::True);
        assert_eq!(t("SP = 0"), Cond::False);
        assert_eq!(t("SP <> 0"), Cond::True);
        assert_eq!(t("N1234 >< N1"), Cond::False);
        assert_eq!(t("N1234 << N1"), Cond::False);
        assert_eq!(t("NOPE >< NOPE2"), Cond::False);
        assert_eq!(t("NOPE << NOPE2"), Cond::False);
        assert_eq!(t("HI >< NOPE"), Cond::True);
        assert_eq!(t("HI << NOPE"), Cond::True);
    }

    #[test]
    fn basics() {
        let t = |s: &str| evaluate(s, &E);
        assert_eq!(t(""), Cond::None);
        assert_eq!(t("Not Installed"), Cond::True);
        assert_eq!(t("VersionNT >= 500"), Cond::True);
        assert_eq!(t("VersionNT = 400"), Cond::False);
        assert_eq!(t("VersionNT"), Cond::True);
        assert_eq!(t("ProductName = \"Game\""), Cond::True);
        assert_eq!(t("ProductName ~= \"GAME\""), Cond::True);
        assert_eq!(t("ProductName >< \"am\""), Cond::True);
        assert_eq!(t("ProductName << \"Ga\" AND ProductName >> \"me\""), Cond::True);
        assert_eq!(t("ProductName = 1"), Cond::False);
        assert_eq!(t("ProductName <> 1"), Cond::True);
        assert_eq!(t("$C1 = 3 AND ?C1 = -1"), Cond::True);
        assert_eq!(t("(1 OR 0) AND NOT 0"), Cond::True);
        assert_eq!(t("1 XOR 1"), Cond::False);
        assert_eq!(t("0 IMP 0"), Cond::True);
        assert_eq!(t("1 EQV 0"), Cond::False);
        assert_eq!(t("ONE = \"1\""), Cond::True);
        assert_eq!(t("(1"), Cond::Error);
        assert_eq!(t("1 AND"), Cond::Error);
        assert_eq!(t("\"x"), Cond::Error);
    }

    struct NoProps;
    impl Env for NoProps {
        fn property(&self, _: &str) -> String {
            String::new()
        }
    }

    /// NOT and parentheses nested past MAX_DEPTH are an error, not a host stack overflow
    /// (the guest's condition string drove one recursion per level; review b04).
    #[test]
    fn deep_nesting_is_an_error() {
        for n in [MAX_DEPTH + 1, 10_000, 200_000] {
            assert_eq!(evaluate(&format!("{}1", "NOT ".repeat(n)), &NoProps), Cond::Error);
            assert_eq!(evaluate(&format!("{}1{}", "(".repeat(n), ")".repeat(n)), &NoProps), Cond::Error);
        }
        assert_eq!(evaluate(&format!("{}1", "NOT ".repeat(MAX_DEPTH)), &NoProps), Cond::True);
        assert_eq!(evaluate(&format!("{}1{}", "(".repeat(MAX_DEPTH), ")".repeat(MAX_DEPTH)), &NoProps), Cond::True);
        // (levels left again do not count: siblings each MAX_DEPTH deep)
        let one = format!("{}1{}", "(".repeat(MAX_DEPTH), ")".repeat(MAX_DEPTH));
        assert_eq!(evaluate(&[one.as_str(); 50].join(" AND "), &NoProps), Cond::True);
    }

    proptest::proptest! {
        /// Any mix of NOTs and parentheses within the limit: the value is 1 negated once
        /// per NOT; past the limit an error.
        #[test]
        fn nesting_model(ops in proptest::collection::vec(proptest::prelude::any::<bool>(), 0..400)) {
            let mut s = String::new();
            let mut close = 0;
            let (mut depth, mut max, mut nots) = (0usize, 0usize, 0usize);
            for &not in &ops {
                if not {
                    s.push_str("NOT ");
                    nots += 1;
                } else {
                    s.push('(');
                    close += 1;
                }
                depth += 1;
                max = max.max(depth);
            }
            s.push('1');
            s.push_str(&")".repeat(close));
            let want = if max > MAX_DEPTH { Cond::Error } else if nots % 2 == 0 { Cond::True } else { Cond::False };
            proptest::prop_assert_eq!(evaluate(&s, &NoProps), want);
        }
    }
}
