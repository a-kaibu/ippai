//! sake-shirusu LOG OUT.trace [--arch x86|x86_64] [--target NAME]
//!   LOG: sake's stderr or a page's console.log with SAKE_SHIRUSU=1 (crates/sake/src/shirusu.rs)
//!   prints the calls per API; exit 1 when the log has no calls or does not convert
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut pos = Vec::new();
    let mut opt = sake_shirusu::Options { arch: "x86".into(), created_by: format!("sake (masu) sake-shirusu {}", env!("CARGO_PKG_VERSION")), target: String::new() };
    let mut i = 0;
    while i < a.len() {
        match a[i].as_str() {
            "--arch" => { i += 1; opt.arch = a.get(i).cloned().unwrap_or_default(); }
            "--target" => { i += 1; opt.target = a.get(i).cloned().unwrap_or_default(); }
            x => pos.push(x.to_string()),
        }
        i += 1;
    }
    if pos.len() != 2 || !matches!(opt.arch.as_str(), "x86" | "x86_64") {
        eprintln!("usage: sake-shirusu LOG OUT.trace [--arch x86|x86_64] [--target NAME]");
        std::process::exit(2);
    }
    let log = std::fs::read(&pos[0]).unwrap_or_else(|e| { eprintln!("{}: {e}", pos[0]); std::process::exit(1) });
    match sake_shirusu::convert(&String::from_utf8_lossy(&log), &opt) {
        Ok((bytes, st)) => {
            std::fs::write(&pos[1], bytes).unwrap_or_else(|e| { eprintln!("{}: {e}", pos[1]); std::process::exit(1) });
            println!("{} calls ({} unfinished, {} bad lines)", st.calls, st.unfinished, st.bad);
            for (k, n) in &st.apis { println!("  {k} {n}"); }
            if st.calls == 0 { std::process::exit(1); }
        }
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    }
}
