//! The metadata entry (meta.rs `lookup`'s pinned id) a pasted address names: a game's page on
//! ErogameScape (批評空間) or VNDB, as the browser shows it, or the id itself.
//!
//! - `https://erogamescape.dyndns.org/~ap2/ero/toukei_kaiseki/game.php?game=12345` (any
//!   erogamescape.dyndns.org / erogamescape.org host, http or https, other parameters and a
//!   `#...` allowed) -> `egs:12345`
//! - `https://vndb.org/v1195` (also `/v1195/chars`, `?...`, `#...`) -> `vndb:v1195`
//! - `egs:12345`, `vndb:v1195` as they are
//!
//! Anything else is `None`. Self-contained (the fuzz target includes this file).

/// A game id: 1 to 9 ASCII digits naming a number over 0, without its leading zeros.
fn number(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 9 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok().filter(|&n| n > 0)
}

fn vndb(s: &str) -> Option<String> {
    let n = number(s.strip_prefix('v').or_else(|| s.strip_prefix('V'))?)?;
    Some(format!("vndb:v{n}"))
}

/// `egs:<n>` or `vndb:v<n>` for `s`, or `None` when it names no game page.
pub fn entry_id(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() > 2048 {
        return None;
    }
    if let Some(n) = s.strip_prefix("egs:") {
        return number(n).map(|n| format!("egs:{n}"));
    }
    if let Some(v) = s.strip_prefix("vndb:") {
        return vndb(v);
    }
    let lower = s.to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &s[8..]
    } else if lower.starts_with("http://") {
        &s[7..]
    } else {
        s
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, rest) = rest.split_at(end);
    if authority.contains('@') {
        return None;
    }
    let host = authority.split(':').next().unwrap_or("").to_ascii_lowercase();
    let host = host.trim_end_matches('.');
    let host = host.strip_prefix("www.").unwrap_or(host);
    let rest = rest.split('#').next().unwrap_or("");
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    match host {
        "erogamescape.dyndns.org" | "erogamescape.org" => {
            if !path.ends_with("/game.php") {
                return None;
            }
            let game = query.split('&').find_map(|p| p.strip_prefix("game="))?;
            number(game).map(|n| format!("egs:{n}"))
        }
        "vndb.org" => vndb(path.trim_start_matches('/').split('/').next().unwrap_or("")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::entry_id;
    use proptest::prelude::*;

    #[test]
    fn pages() {
        for line in include_str!("../../../testdata/meta-urls.tsv").lines().filter(|l| !l.starts_with('#')) {
            let (url, want) = line.split_once('\t').unwrap();
            let url = url.replace("\\n", "\n").replace("\\t", "\t");
            let want = Some(want).filter(|w| *w != "-");
            assert_eq!(entry_id(&url).as_deref(), want, "{url:?}");
        }
    }

    proptest! {
        /// Every id comes back from its page, with whatever parameters and fragment around it.
        #[test]
        fn round_trip(n in 1u32..=999_999_999, before in "[a-z]{0,4}", after in "[a-z0-9=]{0,6}", frag in "[^#]{0,8}") {
            let egs = format!("https://erogamescape.dyndns.org/~ap2/ero/toukei_kaiseki/game.php?{before}{}game={n}&{after}#{frag}", if before.is_empty() { "" } else { "=1&" });
            prop_assert_eq!(entry_id(&egs), Some(format!("egs:{n}")));
            let v = format!("https://vndb.org/v{n}/{after}#{frag}");
            prop_assert_eq!(entry_id(&v), Some(format!("vndb:v{n}")));
            prop_assert_eq!(entry_id(&format!("egs:{n}")), Some(format!("egs:{n}")));
        }

        /// Any text: no panic, and an answer is always an id `lookup` takes.
        #[test]
        fn anything(s in "\\PC{0,80}") {
            if let Some(id) = entry_id(&s) {
                let ok = id.strip_prefix("egs:").or(id.strip_prefix("vndb:v")).is_some_and(|n| n.parse::<u32>().is_ok_and(|n| n > 0) && !n.starts_with('0'));
                prop_assert!(ok, "{id}");
            }
        }
    }
}
