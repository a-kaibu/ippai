//! The clauses (spec/*.txt, spec/README.md): what each API does, numbered, with its status
//! and source. The models name each clause they apply (`hit`); a case that reaches an `open`
//! clause is unmodelled (Fail::Unmodeled(id)). The coverage of a run is the set of clauses hit.

use std::cell::RefCell;
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq)]
pub struct Clause {
    pub id: String,
    pub status: String,
    pub source: String,
    pub text: String,
    pub file: &'static str,
}

pub const FILES: &[(&str, &str)] = &[("gdi", include_str!("../spec/gdi.txt")), ("d3d8", include_str!("../spec/d3d8.txt")), ("d3d9", include_str!("../spec/d3d9.txt")), ("d3d11", include_str!("../spec/d3d11.txt")), ("wic", include_str!("../spec/wic.txt"))];

/// (excluded: deliberately not asked, e.g. calls that read past Windows' own arrays)
pub const STATUSES: &[&str] = &["doc", "measured", "assumed", "open", "excluded"];

pub fn clauses() -> Vec<Clause> {
    let mut v = Vec::new();
    for (file, text) in FILES {
        for l in text.lines() {
            if l.starts_with('#') || l.trim().is_empty() {
                continue;
            }
            let mut parts = l.splitn(3, " | ");
            let head = parts.next().unwrap_or("");
            let (id, status) = head.split_once(' ').unwrap_or((head, ""));
            v.push(Clause { id: id.into(), status: status.trim().into(), source: parts.next().unwrap_or("").into(), text: parts.next().unwrap_or("").into(), file });
        }
    }
    v
}

thread_local! {
    static HITS: RefCell<BTreeSet<&'static str>> = const { RefCell::new(BTreeSet::new()) };
}

/// the model applies clause `id`
pub fn hit(id: &'static str) {
    HITS.with(|h| h.borrow_mut().insert(id));
}

/// the clauses hit since the last call
pub fn take() -> BTreeSet<&'static str> {
    HITS.with(|h| std::mem::take(&mut *h.borrow_mut()))
}

/// every suite's cases through the models: clause -> the cases ("suite:case") that reach it
pub fn coverage() -> std::collections::BTreeMap<String, Vec<String>> {
    let mut m: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for s in crate::gen::SUITES {
        for c in crate::case::parse_cases(&crate::gen::suite(s).unwrap()) {
            take();
            let _ = crate::model::run_case(&c);
            for id in take() {
                m.entry(id.to_string()).or_default().push(format!("{s}:{}", c.name));
            }
        }
    }
    m
}
