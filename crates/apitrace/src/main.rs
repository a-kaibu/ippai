//! apitrace <trace> <stream>: an apitrace D3D9 trace (32-bit, recorded on Windows) as the call
//! stream tests/replay/d3d9replay.c plays back (docs/replay.md); apitrace dump <trace>: its calls
fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() == 3 && a[1] == "dump" {
        let file = std::fs::read(&a[2]).unwrap_or_else(|e| panic!("{}: {e}", a[2]));
        let ev = apitrace::trace::unpack(&file).unwrap();
        let mut p = apitrace::trace::Parser::new(&ev).unwrap();
        println!("{:?}", p.properties);
        while let Some(c) = p.next_call().unwrap() {
            println!("{} {}{} in {:?} out {:?} ret {:?}", c.no, c.sig.name, if c.fake { " (fake)" } else { "" }, c.args_in, c.args_out, c.ret);
        }
        return;
    }
    if a.len() != 3 {
        eprintln!("usage: apitrace <in.trace> <out.rpl>");
        std::process::exit(2);
    }
    let run = || -> Result<(), String> {
        let file = std::fs::read(&a[1]).map_err(|e| format!("{}: {e}", a[1]))?;
        let (out, stats) = apitrace::replay::convert(&apitrace::trace::unpack(&file)?)?;
        std::fs::write(&a[2], out).map_err(|e| format!("{}: {e}", a[2]))?;
        eprintln!("{}: {} calls, {} replayed, {} memcpy", a[1], stats.calls, stats.written, stats.memcpys);
        for (name, n) in &stats.dropped {
            eprintln!("  not replayed: {name} x{n}");
        }
        Ok(())
    };
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
