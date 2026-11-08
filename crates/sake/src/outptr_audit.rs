//! Out-parameters that point at something pointer-sized (pointers, handles, interfaces, SIZE_T,
//! SRWLOCK...) are 8 bytes on x64; a handler that writes them with put32/w32 leaves the upper half
//! as the caller had it (GetProcessAffinityMask's masks, InitializeSRWLock's lock, every
//! DirectDrawCreate/DirectSoundCreate interface: tests/winref/probes/outptr*.c). outptrs.txt is
//! generated from Microsoft's Win32 metadata (tools/win32md): "dll name i,j..." per function.
//! This checks every `fn Name(c: &mut Ctx) -> u32` handler named after a function there.
use std::collections::HashMap;

/// (handler name, file, body) of every handler in src/.
fn handlers() -> Vec<(String, String, String)> {
    let mut v = vec![];
    let mut dirs = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                dirs.push(p);
                continue;
            }
            if p.extension().is_none_or(|x| x != "rs") {
                continue;
            }
            let s = std::fs::read_to_string(&p).unwrap();
            let mut rest = s.as_str();
            while let Some(at) = rest.find("\nfn ") {
                rest = &rest[at + 4..];
                let head = rest;
                let Some(paren) = head.find('(') else { continue };
                let name = &head[..paren];
                if !head[paren..].starts_with("(c: &mut Ctx) -> u32 {\n") || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    continue;
                }
                let body = &head[paren..];
                let end = body.find("\n}\n").unwrap_or(body.len());
                v.push((name.to_string(), p.display().to_string(), body[..end].to_string()));
            }
        }
    }
    v
}

#[test]
fn pointer_sized_out_params_are_written_whole() {
    let mut table: HashMap<&str, Vec<usize>> = HashMap::new();
    for l in include_str!("outptrs.txt").lines() {
        let v: Vec<&str> = l.split(' ').collect();
        table.entry(v[1]).or_default().extend(v[2].split(',').map(|i| i.parse::<usize>().unwrap()));
    }
    let hs = handlers();
    let checked: Vec<_> = hs.iter().filter(|h| table.contains_key(h.0.as_str())).collect();
    // (the parse above found them: 267 handlers have such parameters at 2026-11)
    assert!(checked.len() > 200, "only {} handlers found", checked.len());
    let mut bad = vec![];
    for (name, file, body) in checked {
        for i in &table[name.as_str()] {
            for w in ["put32", "w32"] {
                let call = format!("{w}(c.a({i})");
                let hit = body.match_indices(&call).any(|(at, _)| at == 0 || !body.as_bytes()[at - 1].is_ascii_alphanumeric() && body.as_bytes()[at - 1] != b'_');
                if hit {
                    bad.push(format!("{file}: {name} argument {i}: {call}...) writes 4 of its 8 bytes on x64 (mem::putp / wp)"));
                }
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
