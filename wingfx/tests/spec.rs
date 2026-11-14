//! The clauses (spec/) against the models and the suites: every clause is reached by some case
//! (a documented rule no case exercises is a hole in the suite), the models name no clause the
//! files lack, statuses are known, ids are unique.
use std::collections::BTreeSet;
use wingfx::spec;

#[test]
fn every_clause_is_reached_and_known() {
    let cl = spec::clauses();
    let ids: BTreeSet<_> = cl.iter().map(|c| c.id.clone()).collect();
    assert_eq!(ids.len(), cl.len(), "duplicate clause ids");
    for c in &cl {
        assert!(spec::STATUSES.contains(&c.status.as_str()), "{}: status {:?}", c.id, c.status);
        assert!(!c.source.is_empty() && !c.text.is_empty(), "{}: source and text", c.id);
    }
    let cov = spec::coverage();
    let unknown: Vec<_> = cov.keys().filter(|k| !ids.contains(*k)).collect();
    assert!(unknown.is_empty(), "the models name clauses spec/ lacks: {unknown:?}");
    let unreached: Vec<_> = cl.iter().filter(|c| !cov.contains_key(&c.id) && c.status != "excluded").map(|c| &c.id).collect();
    assert!(unreached.is_empty(), "clauses no case reaches: {unreached:?}");
    for c in &cl {
        eprintln!("{:24} {:8} {:5} cases", c.id, c.status, cov.get(&c.id).map_or(0, |v| v.len()));
    }
}
