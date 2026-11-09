//! MsiFormatRecord: field 0 is the template; [n] a field's text, [Property], [%ENV],
//! [#file] / [!file] a file's path / short path, [$component] its directory, [\c] the
//! character c, [~] a NUL; brackets nest ([[1]] uses field 1's text as the key); {...}
//! keeps its text only when every property in it was found. Inserted values are not
//! scanned again. A template with field 0 NULL formats as "1: f1 2: f2 ...".

use crate::record::{Field, Record};

pub trait Env {
    /// None: there is no package (hInstall 0): keys other than fields stay as written
    fn property(&self, _name: &str) -> Option<String> {
        None
    }
    fn has_package(&self) -> bool {
        false
    }
    fn env(&self, name: &str) -> String {
        std::env::var(name).unwrap_or_default()
    }
    fn file(&self, _key: &str, _short: bool) -> Option<String> {
        None
    }
    fn component(&self, _key: &str) -> Option<String> {
        None
    }
}

pub struct NoPackage;
impl Env for NoPackage {}

struct F<'a> {
    rec: &'a Record,
    env: &'a dyn Env,
    /// formatting a field's own text: [n] stays as written
    in_field: bool,
}

/// What the keys of a {group} came to.
#[derive(Default)]
struct St {
    /// a key stayed as written ([dummy] without a package, [-1], [])
    unresolved: bool,
    /// a key came out empty (a NULL field, a property not set)
    empty: bool,
    keys: bool,
}

/// The ']' of the '[' at `i` (brackets nest; "[\c" escapes c), or None.
fn close_bracket(s: &[char], i: usize) -> Option<usize> {
    let mut depth = 0;
    let mut j = i;
    while j < s.len() {
        match s[j] {
            '[' => {
                depth += 1;
                if s.get(j + 1) == Some(&'\\') {
                    j += 2;
                }
            }
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

fn find2(s: &[char], from: usize, a: char, b: char) -> Option<usize> {
    (from..s.len().saturating_sub(1)).find(|&k| s[k] == a && s[k + 1] == b)
}

impl F<'_> {
    /// The value of a key, or None to write the brackets back.
    fn resolve(&self, key: &str, st: &mut St) -> Option<String> {
        st.keys = true;
        let v = self.value(key);
        match &v {
            None => st.unresolved = true,
            Some(v) if v.is_empty() => st.empty = true,
            _ => {}
        }
        v
    }

    fn value(&self, key: &str) -> Option<String> {
        if key.is_empty() {
            return None;
        }
        if key.bytes().all(|b| b.is_ascii_digit()) {
            if self.in_field {
                return None;
            }
            let n: usize = key.parse().ok()?;
            return Some(match self.rec.get(n) {
                Field::Null => String::new(),
                f => self.field(&f.text()),
            });
        }
        // (without a package only fields are keys)
        if !self.env.has_package() {
            return None;
        }
        let first = key.chars().next()?;
        let rest = &key[first.len_utf8()..];
        Some(match first {
            '\\' => rest.chars().next().map(|c| c.to_string())?,
            '~' if rest.is_empty() => "\0".into(),
            '%' => self.env.env(rest),
            '#' => self.env.file(rest, false).unwrap_or_default(),
            '!' => self.env.file(rest, true).unwrap_or_default(),
            '$' => self.env.component(rest).unwrap_or_default(),
            _ => self.env.property(key).unwrap_or_default(),
        })
    }

    /// A '{' at `s[i]` outside a group: "{{" drops everything up to the next "}}" (with no
    /// "}}" after it, braces are text from there on); "{...}" is a group up to the first '}'.
    /// Returns (text, the index after it, whether braces are now text).
    fn brace(&self, s: &[char], i: usize) -> (String, usize, bool) {
        if s.get(i + 1) == Some(&'{') {
            return match find2(s, i + 2, '}', '}') {
                Some(k) => (String::new(), k + 2, false),
                None => ("{".into(), i + 1, true),
            };
        }
        match s[i + 1..].iter().position(|&c| c == '}') {
            Some(k) => (self.group(&s[i + 1..i + 1 + k]), i + 2 + k, false),
            None => ("{".into(), i + 1, false),
        }
    }

    /// A group's text: with a key written back, its braces stay; with a key empty, nothing;
    /// with no keys, the text in its braces (nothing for "{}"); else its text.
    fn group(&self, s: &[char]) -> String {
        let mut st = St::default();
        let mut out = String::new();
        let mut i = 0;
        while i < s.len() {
            if s[i] == '[' {
                // (a bracket that does not close inside the group: the group is text)
                let Some(j) = close_bracket(s, i) else {
                    return format!("{{{}}}", s.iter().collect::<String>());
                };
                out += &self.bracket(&s[i + 1..j], &mut st);
                i = j + 1;
            } else {
                out.push(s[i]);
                i += 1;
            }
        }
        if st.unresolved {
            format!("{{{out}}}")
        } else if st.empty {
            String::new()
        } else if !st.keys {
            if s.is_empty() { String::new() } else { format!("{{{out}}}") }
        } else {
            out
        }
    }

    /// A bracket's text (`s`: between its brackets). Brackets in it are resolved first and
    /// make its key; a group in it makes it text: what came before the group as written,
    /// the rest formatted, in brackets.
    fn bracket(&self, s: &[char], st: &mut St) -> String {
        let mut key = String::new();
        let mut frozen: Option<String> = None;
        let mut i = 0;
        if s.first() == Some(&'\\') && s.len() >= 2 {
            key.push(s[0]);
            key.push(s[1]);
            i = 2;
        }
        while i < s.len() {
            match s[i] {
                '[' => {
                    let j = close_bracket(s, i).unwrap_or(s.len() - 1);
                    key += &self.bracket(&s[i + 1..j.max(i + 1).min(s.len())], st);
                    i = j + 1;
                }
                '{' => {
                    if frozen.is_none() {
                        frozen = Some(s[..i].iter().collect());
                        key.clear();
                    }
                    let (t, next, _) = self.brace(s, i);
                    key += &t;
                    i = next;
                }
                c => {
                    key.push(c);
                    i += 1;
                }
            }
        }
        if let Some(f) = frozen {
            return format!("[{f}{key}]");
        }
        match self.resolve(&key, st) {
            Some(v) => v,
            None => format!("[{key}]"),
        }
    }

    /// A field's value: with a package its own keys are formatted too (not its [n]).
    fn field(&self, t: &str) -> String {
        if !self.env.has_package() || !t.contains('[') {
            return t.to_string();
        }
        let c: Vec<char> = t.chars().collect();
        F { rec: self.rec, env: self.env, in_field: true }.run(&c)
    }

    fn run(&self, s: &[char]) -> String {
        let mut out = String::new();
        let mut text_braces = false;
        let mut st = St::default();
        let mut i = 0;
        while i < s.len() {
            match s[i] {
                '[' => match close_bracket(s, i) {
                    // (a '[' that never closes: the rest is text)
                    None => {
                        out.extend(&s[i..]);
                        break;
                    }
                    Some(j) => {
                        out += &self.bracket(&s[i + 1..j], &mut st);
                        i = j + 1;
                    }
                },
                '{' if !text_braces => {
                    let (t, next, tb) = self.brace(s, i);
                    out += &t;
                    text_braces = tb;
                    i = next;
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        out
    }
}

pub fn format_record(rec: &Record, env: &dyn Env) -> String {
    match rec.get(0) {
        Field::Null => {
            let f = F { rec, env, in_field: false };
            let mut s = String::new();
            for i in 1..=rec.count() {
                s.push_str(&format!("{i}: {} ", f.field(&rec.get(i).text())));
            }
            s
        }
        f => format_str(&f.text(), rec, env),
    }
}

pub fn format_str(t: &str, rec: &Record, env: &dyn Env) -> String {
    let c: Vec<char> = t.chars().collect();
    F { rec, env, in_field: false }.run(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(t: &str, f: &[(usize, &str)]) -> Record {
        let mut r = Record::new(12);
        r.set(0, Field::Str(t.into())).unwrap();
        for &(i, s) in f {
            r.set(i, Field::Str(s.into())).unwrap();
        }
        r
    }

    #[test]
    fn wine_cases() {
        let n = &NoPackage;
        let mut r = Record::new(4);
        assert_eq!(format_record(&r, n), "1:  2:  3:  4:  ");
        r.set(0, Field::Str("%1".into())).unwrap();
        assert_eq!(format_record(&r, n), "%1");
        assert_eq!(format_record(&rec("[1]", &[]), n), "");
        assert_eq!(format_record(&rec("[1] [2]", &[(1, "boo"), (2, "hoo")]), n), "boo hoo");
        assert_eq!(format_record(&rec("[1] test [0]", &[(1, "boo")]), n), "boo test [1] test [0]");
        assert_eq!(format_record(&rec("[[1]] [2]", &[(1, "2"), (2, "hey")]), n), "hey hey");
        assert_eq!(format_record(&rec("[[1]] [2]", &[(1, "[2]"), (2, "hey")]), n), "[[2]] hey");
        assert_eq!(format_record(&rec("[[[3]]] [2]", &[(1, "2"), (2, "hey"), (3, "1")]), n), "hey hey");
        assert_eq!(format_record(&rec("[[3][1]] [2]", &[(1, "2"), (2, "hey"), (3, "1"), (12, "big")]), n), "big hey");
        assert_eq!(format_record(&rec("[[3][[4]][1]] [2]", &[(1, "2"), (2, "hey"), (3, "1"), (12, "big")]), n), "[1[]2] hey");
        // (Windows' answers for malformed templates, from Wine's format tests)
        let f = &[(1, "2"), (2, "hey"), (3, "1"), (12, "big")];
        for (t, want) in [
            ("[[[3][[4]][1]] [2]", "[[[3][[4]][1]] [2]"),
            ("[{[3][1]}] [2]", "[12] hey"),
            ("{[test]} [2]", "{[test]} hey"),
            ("{[1][2][3][4][dummy]} [2]", "{2hey1[dummy]} hey"),
            ("{{[1][2]}[3][4][dummy]}", "{{2hey}1[dummy]}"),
            ("{{[1][2]}[3]{[4][dummy]}}", ""),
        ] {
            assert_eq!(format_record(&rec(t, f), n), want, "{t}");
        }
        let f = &[(1, "1"), (2, "2"), (3, "3")];
        for (t, want) in [
            ("{[4]}{[1][2]} {{[1][2]}[3]} {[1][2]}", "12 {{12}3} {12}"),
            ("{{[1]}[2]} {[4]}{[1][2]}", "{{1}2} {}{12}"),
            ("{{{[1]}} {[4]}{[1][2]}", " 12"),
        ] {
            assert_eq!(format_record(&rec(t, f), n), want, "{t}");
        }
        let f = &[(1, "hoo")];
        for (t, want) in [
            ("[\\x]", "[\\x]"),
            ("[~]", "[~]"),
            ("{}", ""),
            ("{ {[1]}}", " {hoo}"),
            ("{{[1]} }", "{{hoo} }"),
            (" {{a}}}", " }"),
            ("{{ almost {{ any }} text }}", " text }}"),
            ("0{1{2{3{4[1]5}6}7}8}9", "01{2{3{4hoo56}7}8}9"),
            ("{[1.} [1]", "{[1.} hoo"),
            ("{[{[1]}]}", "{[{[1]}]}"),
            ("[{{boo}}1]", "[1]"),
            ("{[1]{{boo} }}", "hoo{{boo }}"),
            ("[a[1]b", "[a[1]b"),
            ("{{{{}}}}", "}}"),
        ] {
            assert_eq!(format_record(&rec(t, f), n), want, "{t}");
        }
        let f = &[(1, "2")];
        for (t, want) in [("[[1]{}]", "[[1]]"), ("[[1]{}[1]]", "[[1]2]"), ("[a[1]b[1]c{}d[1]e]", "[a[1]b[1]cd2e]")] {
            assert_eq!(format_record(&rec(t, f), n), want, "{t}");
        }
    }

    struct P;
    impl Env for P {
        fn has_package(&self) -> bool {
            true
        }
        fn property(&self, n: &str) -> Option<String> {
            Some(if n == "INSTALLDIR" { "C:\\Game\\".into() } else { String::new() })
        }
    }

    #[test]
    fn properties_and_groups() {
        let r = rec("[INSTALLDIR]game.exe{ /x [NOPE]}{ /y [INSTALLDIR]}[\\[]", &[]);
        assert_eq!(format_record(&r, &P), "C:\\Game\\game.exe /y C:\\Game\\[");
        assert_eq!(format_record(&rec("{abc}", &[]), &P), "{abc}");
    }

    /// With a package a field's own keys are formatted (its [n] stay), with field 0
    /// or without (Wine's test_formatrecord_tables, test_formatrecord_package).
    #[test]
    fn fields_formatted_with_a_package() {
        let r = rec("1: [1] ", &[(1, "[INSTALLDIR]")]);
        assert_eq!(format_record(&r, &P), "1: C:\\Game\\ ");
        assert_eq!(format_record(&r, &NoPackage), "1: [INSTALLDIR] ");
        let mut r = Record::new(5);
        for (i, v) in [(1, "[2]"), (2, "stuff"), (3, "prop"), (4, "[INSTALLDIR]"), (5, "[noprop]")] {
            r.set(i, Field::Str(v.into())).unwrap();
        }
        assert_eq!(format_record(&r, &P), "1: [2] 2: stuff 3: prop 4: C:\\Game\\ 5:  ");
        assert_eq!(format_record(&r, &NoPackage), "1: [2] 2: stuff 3: prop 4: [INSTALLDIR] 5: [noprop] ");
        r.set(0, Field::Str("[1]".into())).unwrap();
        assert_eq!(format_record(&r, &P), "[2]");
    }
}
