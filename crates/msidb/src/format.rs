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
    c: Vec<char>,
    i: usize,
    rec: &'a Record,
    env: &'a dyn Env,
    /// a property or field in the current {group} came out empty
    missing: bool,
    any_key: bool,
}

impl F<'_> {
    /// The value of a key, or None to write the brackets back.
    fn resolve(&mut self, key: &str) -> Option<String> {
        if key.is_empty() {
            return None;
        }
        self.any_key = true;
        if key.bytes().all(|b| b.is_ascii_digit()) {
            let n: usize = key.parse().ok()?;
            let v = match self.rec.get(n) {
                Field::Null => String::new(),
                f => f.text(),
            };
            if v.is_empty() {
                self.missing = true;
            }
            return Some(v);
        }
        let first = key.chars().next()?;
        let rest = &key[first.len_utf8()..];
        if !self.env.has_package() {
            return match first {
                '\\' if rest.chars().count() >= 1 => Some(rest.chars().next().unwrap().to_string()),
                '~' if rest.is_empty() => Some("\0".into()),
                _ => None,
            };
        }
        let v = match first {
            '\\' => return rest.chars().next().map(|c| c.to_string()),
            '~' if rest.is_empty() => return Some("\0".into()),
            '%' => self.env.env(rest),
            '#' => self.env.file(rest, false).unwrap_or_default(),
            '!' => self.env.file(rest, true).unwrap_or_default(),
            '$' => self.env.component(rest).unwrap_or_default(),
            _ => self.env.property(key).unwrap_or_default(),
        };
        if v.is_empty() {
            self.missing = true;
        }
        Some(v)
    }

    /// After a '[': the bracket's text up to its ']' (nested brackets resolved), resolved.
    fn bracket(&mut self) -> String {
        let start = self.i;
        let mut key = String::new();
        while self.i < self.c.len() {
            let ch = self.c[self.i];
            match ch {
                ']' => {
                    self.i += 1;
                    return match self.resolve(&key) {
                        Some(v) => v,
                        None => format!("[{key}]"),
                    };
                }
                '[' => {
                    self.i += 1;
                    key.push_str(&self.bracket());
                }
                '\\' if key.is_empty() && self.i + 1 < self.c.len() => {
                    key.push(ch);
                    key.push(self.c[self.i + 1]);
                    self.i += 2;
                }
                _ => {
                    key.push(ch);
                    self.i += 1;
                }
            }
        }
        // no ']': the '[' and the rest are text
        self.i = start;
        "[".into()
    }

    fn group(&mut self) -> String {
        let start = self.i;
        let (m, k) = (self.missing, self.any_key);
        self.missing = false;
        self.any_key = false;
        let mut out = String::new();
        while self.i < self.c.len() {
            let ch = self.c[self.i];
            self.i += 1;
            match ch {
                '}' => {
                    let r = if !self.any_key {
                        format!("{{{out}}}")
                    } else if self.missing {
                        String::new()
                    } else {
                        out
                    };
                    self.missing = m;
                    self.any_key = k;
                    return r;
                }
                '[' => out.push_str(&self.bracket()),
                '{' => out.push_str(&self.group()),
                c => out.push(c),
            }
        }
        self.i = start;
        self.missing = m;
        self.any_key = k;
        "{".into()
    }

    fn run(&mut self) -> String {
        let mut out = String::new();
        while self.i < self.c.len() {
            let ch = self.c[self.i];
            self.i += 1;
            match ch {
                '[' => out.push_str(&self.bracket()),
                '{' => out.push_str(&self.group()),
                c => out.push(c),
            }
        }
        out
    }
}

pub fn format_record(rec: &Record, env: &dyn Env) -> String {
    match rec.get(0) {
        Field::Null => {
            let mut s = String::new();
            for i in 1..=rec.count() {
                s.push_str(&format!("{i}: {} ", rec.get(i).text()));
            }
            s
        }
        f => format_str(&f.text(), rec, env),
    }
}

pub fn format_str(t: &str, rec: &Record, env: &dyn Env) -> String {
    F { c: t.chars().collect(), i: 0, rec, env, missing: false, any_key: false }.run()
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
}
