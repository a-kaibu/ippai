//! wingfx command line (README.md):
//!   wingfx gen <suite>                  the suite's case file (cases/<suite>.txt)
//!   wingfx model <suite> [cases file]   what Windows prints for them, by the model
//!   wingfx compare <want> <got>         two outputs, case by case (exit 1 on a difference)
use std::process::exit;
use wingfx::{case, compare, gen, model};

fn read(p: &str) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| {
        eprintln!("{p}: {e}");
        exit(2)
    })
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["gen", suite] => print!("{}", gen::suite(suite).unwrap_or_else(|| {
            eprintln!("suites: {:?}", gen::SUITES);
            exit(2)
        })),
        ["model", suite, rest @ ..] => {
            let text = match rest {
                [f] => read(f),
                _ => gen::suite(suite).unwrap_or_default(),
            };
            print!("{}", model::run(suite, &case::parse_cases(&text)));
        }
        ["compare", want, got] => {
            let r = compare::compare_text(&read(want), &read(got));
            for d in &r.differ {
                println!("{d}");
            }
            println!("{} cases: {} same, {} differ, {} unmodeled", r.cases, r.same, r.differ.len(), r.unmodeled);
            exit(if r.differ.is_empty() { 0 } else { 1 })
        }
        _ => {
            eprintln!("usage: wingfx gen <suite> | model <suite> [cases] | compare <want> <got>");
            exit(2)
        }
    }
}
