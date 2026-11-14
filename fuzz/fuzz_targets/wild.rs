//! sake::files::wild (FindFirstFile's DOS wildcards, a table since review b06: the recursion
//! went exponential) on any pattern and name: no panic, in time; "*" and "*.*" match every
//! name; a name matches itself (no wildcards in it) and its upper case.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|d: &[u8]| {
    let s = String::from_utf8_lossy(d);
    let (p, n) = s.split_once('\0').unwrap_or((&s, ""));
    // (at most 300 characters each: MAX_PATH, cut on a character boundary)
    let cut = |t: &str| t.chars().take(300).collect::<String>();
    let (p, n) = (cut(p), cut(n));
    let (p, n) = (p.as_str(), n.as_str());
    let _ = sake::files::wild(p, n);
    assert!(sake::files::wild("*", n));
    assert!(sake::files::wild("*.*", n));
    if !n.contains(['*', '?']) && !n.ends_with('.') && !n.is_empty() {
        assert!(sake::files::wild(n, n), "{n:?}");
        if n.is_ascii() {
            assert!(sake::files::wild(&n.to_ascii_uppercase(), n), "{n:?}");
        }
    }
});
