//! The models: what Windows prints for a case file, in the executor's own output format. A case
//! the model cannot answer prints "unmodeled <why>" in place of its lines (and is then compared
//! with Windows only).

pub mod d3d11;
pub mod d3d9;
pub mod gdi;
pub mod wic;

use crate::case::Case;

#[derive(Debug, Clone, PartialEq)]
pub enum Fail {
    /// a line the executor would print "bad line N" for (the generators never write one)
    Bad,
    /// the clause (spec/*.txt, status open) that does not settle what Windows does here
    Unmodeled(&'static str),
}

pub type Res<T> = Result<T, Fail>;

/// one case's lines, or why the model has none
pub fn run_case(case: &Case) -> Res<Vec<String>> {
    let r = run_ops(case);
    if let Err(Fail::Unmodeled(id)) = &r {
        crate::spec::hit(id);
    }
    r
}

fn run_ops(case: &Case) -> Res<Vec<String>> {
    let mut out = Vec::new();
    let mut g = gdi::Gdi::new();
    let mut d = d3d9::D3d9::default();
    let mut d11 = d3d11::D3d11::default();
    for op in &case.ops {
        if g.op(op, &mut out)? {
            continue;
        }
        if d.op(op, &mut out)? {
            continue;
        }
        if d11.op(op, &mut out)? {
            continue;
        }
        if wic::op(op, &mut out)? {
            continue;
        }
        return Err(Fail::Bad);
    }
    Ok(out)
}

/// the whole output for a case file of `suite`
pub fn run(suite: &str, cases: &[Case]) -> String {
    let mut s = format!("wingfx {suite}\n");
    for c in cases {
        s += &format!("case {}\n", c.name);
        match run_case(c) {
            Ok(lines) => lines.iter().for_each(|l| {
                s += l;
                s += "\n"
            }),
            Err(Fail::Bad) => s += "unmodeled bad line\n",
            Err(Fail::Unmodeled(why)) => s += &format!("unmodeled {why}\n"),
        }
    }
    s + "done\n"
}
