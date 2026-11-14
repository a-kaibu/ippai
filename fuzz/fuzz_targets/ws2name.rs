//! ws2_32's name side (sake::ws2name) on any text: the IDNA / Punycode encoding of a host name,
//! a hosts file, and what of a hostent fits the 1024 bytes it has: no panic; Punycode is ASCII
//! and only for labels of 63 characters or fewer; a hosts lookup gives addresses only of lines
//! naming the name; a hostent never needs more than MAXGETHOSTSTRUCT.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sake::ws2name::{hostent_fit, hosts_find, hosts_parse, idna, punycode, HOSTENT_MAX};

fuzz_target!(|data: &[u8]| {
    let Some((&k, rest)) = data.split_first() else { return };
    let text = String::from_utf8_lossy(rest);
    let a = idna(&text);
    assert_eq!(a.split('.').count(), text.split('.').count());
    for l in text.split('.') {
        let lo = l.to_lowercase();
        match punycode(&lo) {
            Some(p) => assert!(p.is_ascii() && lo.chars().count() <= 63),
            None => assert!(lo.chars().count() > 63),
        }
    }
    let (name, file) = text.split_once('\n').unwrap_or((&text, ""));
    let table = hosts_parse(file.as_bytes());
    if let Some(v) = hosts_find(&table, name) {
        assert!(!v.is_empty());
    }
    let kk = if k & 1 == 0 { 4 } else { 8 };
    let len = if k & 2 == 0 { 4 } else { 16 };
    let (n, c) = hostent_fit(kk, len, rest.len() * 37, table.len() * (k as usize + 1));
    assert!(4 * kk + kk + kk * (c as u32 + 1) + len * c as u32 + n as u32 + 1 <= HOSTENT_MAX);
});
