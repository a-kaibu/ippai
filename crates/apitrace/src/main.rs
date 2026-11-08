//! apitrace [-32|-64] [-strict] [-max=<bytes>] <trace> <stream>: an apitrace D3D8 / D3D9 / D3D11 trace (recorded on
//! Windows; cut short where the program was killed, unless -strict) as the call stream tests/replay/replay.c plays back (docs/replay.md);
//! -max: the stream stops at the first call that takes it to that many bytes;
//! apitrace dump <trace>: its calls
fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() == 3 && a[1] == "dump" {
        let file = std::fs::read(&a[2]).unwrap_or_else(|e| panic!("{}: {e}", a[2]));
        let ev = apitrace::trace::unpack(&file).unwrap();
        let mut p = apitrace::trace::Parser::new(&ev).unwrap();
        println!("{:?}", p.properties);
        while let Some(c) = p.next_call().unwrap() {
            println!("{c}");
        }
        return;
    }
    // (-32 / -64: the trace's pointer width, found from its addresses otherwise)
    let bits = a.iter().find_map(|x| match x.as_str() { "-32" => Some(32), "-64" => Some(64), _ => None });
    let max = a.iter().find_map(|x| x.strip_prefix("-max=")).map(|v| v.parse::<usize>().unwrap_or_else(|_| panic!("-max={v}: not a number")));
    let a: Vec<String> = a.into_iter().filter(|x| x != "-32" && x != "-64" && x != "-strict" && !x.starts_with("-max=")).collect();
    if a.len() != 3 {
        eprintln!("usage: apitrace [-32|-64] [-strict] [-max=<bytes>] <in.trace> <out.rpl>");
        std::process::exit(2);
    }
    let run = || -> Result<(), String> {
        let file = std::fs::read(&a[1]).map_err(|e| format!("{}: {e}", a[1]))?;
        // (a game's trace ends where the game was killed: the calls up to the last whole one;
        // -strict: an error)
        let strict = std::env::args().any(|x| x == "-strict");
        let (ev, cut) = apitrace::trace::unpack_cut(&file)?;
        if strict && cut {
            return Err("trace: chunk past the end".into());
        }
        let (out, stats) = apitrace::replay::convert_max(&ev, bits, !strict, max.unwrap_or(usize::MAX))?;
        if stats.limited {
            eprintln!("{}: the stream stops at {} bytes (-max): {} calls of the trace", a[1], out.len(), stats.calls);
        }
        if cut {
            eprintln!("{}: cut short (its last chunk is incomplete): the calls before it", a[1]);
        }
        std::fs::write(&a[2], out).map_err(|e| format!("{}: {e}", a[2]))?;
        eprintln!("{}: {}-bit, {} calls, {} replayed, {} memcpy", a[1], stats.ptrbits, stats.calls, stats.written, stats.memcpys);
        for (name, n) in &stats.dropped {
            eprintln!("  not replayed: {name} x{n}");
        }
        Ok(())
    };
    if let Err(e) = run() {
        eprintln!("{e}");
        // (3: a trace of what the converter does not replay yet: tests/replay/build.sh skips it)
        std::process::exit(if e.starts_with("unsupported:") { 3 } else { 1 });
    }
}
