//! Any bytes as a case file and as executor output: parsing, the models and the comparison
//! never panic, the model answers every case, and an output compared with itself has no
//! difference.
#![no_main]
use libfuzzer_sys::fuzz_target;
use wingfx::{case, compare, model};

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let cases = case::parse_cases(&text);
    let out = model::run("fuzz", &cases);
    let parsed = case::parse_output(&out);
    assert_eq!(parsed.cases.len(), cases.len());
    let r = compare::compare_text(&out, &out);
    assert!(r.differ.is_empty(), "{:?}", r.differ);
    let _ = compare::compare_text(&text, &out);
});
