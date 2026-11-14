//! What a read says the game reads soon, by how game engines lay out their files: the page
//! fetches it onto the device while the game goes on (host/fetch-bridge.js ippaiHint), so that
//! the read that comes later does not wait for the bucket. The bucket (Hugging Face Storage
//! Buckets) answers every object after 0.4-2 s whatever its size (docs/s3-reads.md), and a game
//! reads one file after another: a scene that reads ten voices or pictures the device lacks
//! waited ten times that.
//!
//! Two shapes cover what engines do (vnrun's notes on each engine, docs/s3-reads.md):
//! - loose files in a folder, read in name order (voices "v_0001.ogg", "v_0002.ogg", ...; CG
//!   pictures; scripts): the next files of the folder of the same kind, after the one read.
//! - an archive (one big file, its index then its members): the chunks after the one read, as
//!   members packed side by side are read side by side; a small archive (a script pack read
//!   whole at the start) all of it.
//!
//! Per engine, only how far ahead (Policy): how many neighbours, how many chunks.
//! Movies are never hinted: they are read front to back as they play (vfs.rs SEQ_AHEAD).

/// What a file is, by its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Movie,
    Sound,
    Image,
    Script,
    Archive,
    Other,
}

/// How far ahead a read of a file reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// The folder's next files of the same kind.
    pub siblings: usize,
    /// The file's next chunks (an archive).
    pub chunks: usize,
}

/// Neighbour files at most this big each (one chunk or a few: a voice, a picture).
pub const SIBLING_MAX: u64 = 8 << 20;
/// Bytes of neighbours at most per hint.
pub const SIBLING_BYTES: u64 = 24 << 20;
/// An archive this small is hinted whole at its first read (an index then everything).
pub const WHOLE_MAX: u64 = 16 << 20;

/// The extension of `name`, lower case ("" for none).
fn ext(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, e)) if !stem.is_empty() => e.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// What `name` is.
pub fn kind(name: &str) -> Kind {
    match ext(name).as_str() {
        "mpg" | "mpeg" | "m1v" | "m2v" | "mp4" | "m4v" | "wmv" | "avi" | "webm" | "ogv" | "mkv" | "mov" | "bik" | "usm" | "mjpg" | "dat_mov" => Kind::Movie,
        "ogg" | "wav" | "opus" | "mp3" | "m4a" | "aac" | "flac" | "wma" | "nwa" | "owp" | "wv" => Kind::Sound,
        "png" | "bmp" | "jpg" | "jpeg" | "webp" | "tga" | "gif" | "tlg" | "tlg5" | "tlg6" | "g00" | "pna" | "hg2" | "hg3" | "gyu" | "dds" | "psd" | "cbg" | "crx" | "wip" | "akb" | "bgi" => Kind::Image,
        "ks" | "tjs" | "scn" | "txt" | "ss" | "ybn" | "mjo" | "ast" | "nut" | "ain" | "ini" | "csv" | "xml" | "json" | "lua" | "rpy" | "rpyc" | "sob" | "bin_" => Kind::Script,
        "xp3" | "pck" | "ovk" | "arc" | "int" | "nsa" | "sar" | "ns2" | "pfs" | "rpa" | "assets" | "ress" | "resource" | "bundle" | "ypf" | "pack" | "pac" | "pak" | "dat" | "bin" | "fpk" | "gpk" | "mpk" | "iar" | "war" | "noa" | "cpz" | "npa" | "lpk" | "xfl" | "afs" | "vfs" | "mrg" | "zip" | "afa" | "ald" | "alf" | "med" | "pk" | "dxa" | "gxp" | "cpk" => Kind::Archive,
        _ => Kind::Other,
    }
}

/// How far ahead a read of `name` reaches: by its kind, and per engine by its extension.
pub fn policy(name: &str) -> Policy {
    let p = |siblings, chunks| Policy { siblings, chunks };
    match kind(name) {
        Kind::Movie => p(0, 0),
        // (a scene's lines: read one after another, numbered). The counts are 4x what they were
        // first: replayed against worker2's read-order lists (hints_eval.rs, 2026-11-14) that took
        // the misses from 49% to 37% for 3% more bytes, SIBLING_BYTES capping them
        Kind::Sound => p(48, 1),
        // (a scene's pictures: fewer, bigger)
        Kind::Image => p(24, 1),
        // (small; a script calls the next)
        Kind::Script => p(64, 1),
        Kind::Archive => match ext(name).as_str() {
            // KiriKiri: members packed by folder, a scene reads its pictures and voices from one
            "xp3" => p(0, 4),
            // Siglus: koe/*.ovk hold one character's voices, read by line number (scattered)
            "ovk" => p(0, 1),
            // Unity, Godot, Ren'Py: big files read front to back in pieces at a scene's load
            "assets" | "ress" | "resource" | "bundle" | "rpa" => p(0, 6),
            // Artemis, NScripter, CatSystem2, BGI and the rest: a few ahead
            "pfs" | "nsa" | "sar" | "ns2" | "int" | "ypf" | "pack" | "cpz" => p(0, 3),
            _ => p(0, 2),
        },
        // (start-up files of one folder: .dll, .sig, .exe, fonts)
        Kind::Other => p(16, 1),
    }
}

/// The folder's files after `cur` (`names`: name, size, in the folder's order, which is by name)
/// that a read of `cur` hints: of `cur`'s kind (an unknown kind: its extension), not past
/// SIBLING_MAX each nor SIBLING_BYTES in all, at most `policy(cur).siblings`, the nearest first.
pub fn siblings(names: &[(&str, u64)], cur: &str) -> Vec<usize> {
    siblings_with(names, cur, policy(cur))
}

/// [`siblings`] with policy `pol` instead of `cur`'s (examples/hints_eval.rs tries others).
pub fn siblings_with(names: &[(&str, u64)], cur: &str, pol: Policy) -> Vec<usize> {
    let Some(at) = names.iter().position(|(n, _)| *n == cur) else { return Vec::new() };
    siblings_after(names[at + 1..].iter().enumerate().map(|(i, &(n, size))| (n, size, at + 1 + i)), cur, pol)
}

/// [`siblings_with`] over the files after `cur` as they come (name, size, what to give back
/// for it): a caller with the folder in a sorted map starts right after `cur` and reads no
/// further than the hints go, without a list of the whole folder.
pub fn siblings_after<'a, T>(after: impl Iterator<Item = (&'a str, u64, T)>, cur: &str, pol: Policy) -> Vec<T> {
    let (k, e) = (kind(cur), ext(cur));
    let same = |n: &str| if k == Kind::Other { ext(n) == e } else { kind(n) == k };
    let mut out = Vec::new();
    let mut bytes = 0u64;
    for (n, size, t) in after {
        if out.len() >= pol.siblings {
            break;
        }
        if !same(n) || size == 0 || size > SIBLING_MAX {
            continue;
        }
        if bytes + size > SIBLING_BYTES {
            break;
        }
        bytes += size;
        out.push(t);
    }
    out
}

/// The chunks of a file of `n` chunks (`size` bytes, named `name`) that a read of chunk `i`
/// hints: all of a small archive at its first read, else the next `policy(name).chunks`.
pub fn chunks(name: &str, size: u64, n: usize, i: usize, first: bool) -> std::ops::Range<usize> {
    chunks_with(name, size, n, i, first, policy(name), WHOLE_MAX)
}

/// [`chunks`] with policy `pol` and `whole_max` instead of `name`'s and WHOLE_MAX.
pub fn chunks_with(name: &str, size: u64, n: usize, i: usize, first: bool, pol: Policy, whole_max: u64) -> std::ops::Range<usize> {
    if kind(name) == Kind::Movie || n <= 1 {
        return 0..0;
    }
    if first && kind(name) == Kind::Archive && size <= whole_max {
        return 0..n;
    }
    (i + 1).min(n)..(i + 1 + pol.chunks).min(n)
}

#[cfg(test)]
#[path = "hints_eval.rs"]
mod hints_eval;

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn kinds() {
        assert_eq!(kind("voice/A0001.OGG"), Kind::Sound);
        assert_eq!(kind("data.xp3"), Kind::Archive);
        assert_eq!(kind("op.mpg"), Kind::Movie);
        assert_eq!(kind("g00/BG01.g00"), Kind::Image);
        assert_eq!(kind("scenario/first.ks"), Kind::Script);
        // (AliceSoft, Eushully, MED, DxLib, CRI archives; System4's script)
        for a in ["Rance10CG.afa", "dlcg.ald", "data.alf", "md_scr.med", "misc/script.pk", "DXARC.dxa", "data.gxp", "movie.cpk"] {
            assert_eq!(kind(a), Kind::Archive, "{a}");
        }
        assert_eq!(kind("Rance10.ain"), Kind::Script);
        // (no extension, a dot file)
        assert_eq!(kind("README"), Kind::Other);
        assert_eq!(kind(".ogg"), Kind::Other);
    }

    #[test]
    fn voices_next_in_the_folder() {
        let names = [("a.png", 10), ("v001.ogg", 100), ("v002.ogg", 100), ("v003.ogg", 100), ("v004.wav", 50), ("x.txt", 1)];
        // (sounds of any extension after it; the picture before and the text after are not)
        assert_eq!(siblings(&names, "v002.ogg"), vec![3, 4]);
        assert_eq!(siblings(&names, "v004.wav"), Vec::<usize>::new());
        // (a name not in the folder)
        assert_eq!(siblings(&names, "nope.ogg"), Vec::<usize>::new());
        // (how many: a scene's worth, as replayed against real games)
        let many: Vec<String> = (0..100).map(|i| format!("v{i:03}.ogg")).collect();
        let many: Vec<(&str, u64)> = many.iter().map(|n| (n.as_str(), 1000)).collect();
        assert_eq!(siblings(&many, "v000.ogg"), (1..49).collect::<Vec<_>>());
        for (ext, n) in [("png", 24), ("ks", 64), ("dll", 16)] {
            let names: Vec<String> = (0..100).map(|i| format!("f{i:03}.{ext}")).collect();
            let names: Vec<(&str, u64)> = names.iter().map(|n| (n.as_str(), 1000)).collect();
            assert_eq!(siblings(&names, names[0].0).len(), n, "{ext}");
        }
    }

    #[test]
    fn big_neighbours_left_out_and_bytes_capped() {
        let names = [("a.ogg", 1), ("b.ogg", SIBLING_MAX + 1), ("c.ogg", SIBLING_MAX), ("d.ogg", SIBLING_MAX), ("e.ogg", SIBLING_MAX), ("f.ogg", 1)];
        // (b too big for a neighbour; c, d, e fill the bytes exactly; f past them)
        assert_eq!(siblings(&names, "a.ogg"), vec![2, 3, 4]);
    }

    #[test]
    fn movies_never() {
        let names = [("op.mpg", 100), ("ed.mpg", 100)];
        assert!(siblings(&names, "op.mpg").is_empty());
        assert_eq!(chunks("op.mpg", 100 << 20, 25, 0, true), 0..0);
    }

    #[test]
    fn archives() {
        // (a small one whole at first, a big one the next few)
        assert_eq!(chunks("Scene.pck", 3 << 20, 2, 0, true), 0..2);
        assert_eq!(chunks("data.xp3", 900 << 20, 200, 10, true), 11..15);
        assert_eq!(chunks("data.xp3", 900 << 20, 200, 198, false), 199..200);
        assert_eq!(chunks("koe/z001.ovk", 300 << 20, 70, 5, false), 6..7);
        assert_eq!(chunks("one.xp3", 1 << 20, 1, 0, true), 0..0);
        // (WHOLE_MAX itself is small; one byte more is not)
        assert_eq!(chunks("a.xp3", WHOLE_MAX, 4, 0, true), 0..4);
        assert_eq!(chunks("a.xp3", WHOLE_MAX + 1, 4, 0, true), 1..4);
    }

    proptest! {
        /// Hinted neighbours are after the file read, of its kind, within the caps, in order.
        #[test]
        fn siblings_within_caps(sizes in proptest::collection::vec((0usize..5, 0u64..(12 << 20)), 1..60), cur in 0usize..60) {
            let exts = ["ogg", "png", "ks", "xp3", "zz"];
            let owned: Vec<(String, u64)> = sizes.iter().enumerate().map(|(i, (e, s))| (format!("f{i:03}.{}", exts[*e]), *s)).collect();
            let names: Vec<(&str, u64)> = owned.iter().map(|(n, s)| (n.as_str(), *s)).collect();
            let cur = cur % names.len();
            let got = siblings(&names, names[cur].0);
            let pol = policy(names[cur].0);
            prop_assert!(got.len() <= pol.siblings);
            prop_assert!(got.windows(2).all(|w| w[0] < w[1]));
            let mut bytes = 0;
            for &i in &got {
                prop_assert!(i > cur);
                prop_assert_eq!(ext(names[i].0), ext(names[cur].0));
                prop_assert!(names[i].1 > 0 && names[i].1 <= SIBLING_MAX);
                bytes += names[i].1;
            }
            prop_assert!(bytes <= SIBLING_BYTES);
        }

        /// Hinted chunks are the file's, never the one read (but a small archive's first read: all).
        #[test]
        fn chunks_in_file(n in 0usize..300, i in 0usize..300, size in 0u64..(64 << 20), first: bool, e in 0usize..4) {
            let name = ["a.xp3", "b.ogg", "c.mpg", "d.zz"][e];
            let i = if n == 0 { 0 } else { i % n };
            let r = chunks(name, size, n, i, first);
            prop_assert!(r.end <= n);
            let whole = first && kind(name) == Kind::Archive && size <= WHOLE_MAX && n > 1;
            if !whole {
                prop_assert!(r.is_empty() || r.start == i + 1);
                prop_assert!(r.len() <= policy(name).chunks);
            }
        }
    }
}
