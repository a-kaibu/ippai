//! What a game reads next, foreseen from what it reads now: a script names the pictures, voices
//! and scripts its scene shows and plays ("[bg storage=bg01]", "@voice v_0012"), so when the game
//! reads a script, those files are fetched onto the device before it gets to them (vfs.rs
//! `predict`, the page's ippaiHint). The same way whichever way the game goes: the gallery's
//! script names the gallery's pictures, a route's its scenes, on the game's first run too.
//!
//! The scripts read here: loose ones (a tree's .ks, .txt, .scn ... files), and the members of
//! KiriKiri's .xp3 archives (half the store's games; their index names each member and where its
//! bytes lie: [`xp3_index`]) as they are in the archive or zlib'd, as text plain, UTF-16, or in
//! KiriKiri's own simple text encryption (`FE FE mode FF FE`: [`text`]). A script a game
//! encrypts its own way (cxdec and the like) reads as noise and names nothing; engines with
//! compiled, packed scripts (Siglus' Scene.pck, BGI) name nothing here either.

use super::hints::{kind, Kind};

/// A member of an archive: its name (as the index has it) and its segments, each
/// (offset in the archive, bytes there, zlib'd).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub segments: Vec<(u64, u64, bool)>,
}

const XP3_MAGIC: &[u8; 11] = b"XP3\r\n \n\x1a\x8b\x67\x01";

/// Where an .xp3's index is, from its first bytes (`head`, 32 or more): KiriKiri 2.3's files
/// start with a header pointing past a second one.
pub fn xp3_index_at(head: &[u8]) -> Option<u64> {
    if head.get(..11)? != XP3_MAGIC {
        return None;
    }
    let at = u64::from_le_bytes(head.get(11..19)?.try_into().ok()?);
    // (2.3: 0x17, then a flag 0x80, 8 bytes, then the index's place)
    if at == 0x17 && head.get(0x17) == Some(&0x80) {
        return Some(u64::from_le_bytes(head.get(0x20..0x28)?.try_into().ok()?));
    }
    Some(at)
}

/// The index's bytes at its place (`raw`: from there on, as many as there are): unpacked.
pub fn xp3_index_bytes(raw: &[u8]) -> Option<Vec<u8>> {
    match *raw.first()? {
        0 => {
            let n = u64::from_le_bytes(raw.get(1..9)?.try_into().ok()?) as usize;
            raw.get(9..9usize.checked_add(n)?).map(<[u8]>::to_vec)
        }
        1 => {
            let packed = u64::from_le_bytes(raw.get(1..9)?.try_into().ok()?) as usize;
            let size = u64::from_le_bytes(raw.get(9..17)?.try_into().ok()?) as usize;
            if size > INDEX_MAX {
                return None;
            }
            let out = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(raw.get(17..17usize.checked_add(packed)?)?, size).ok()?;
            (out.len() == size).then_some(out)
        }
        _ => None,
    }
}

/// The bytes an index's place needs to be read from (flag, lengths): `raw`'s first 17.
pub fn xp3_index_len(raw: &[u8]) -> Option<u64> {
    match *raw.first()? {
        0 => Some(9 + u64::from_le_bytes(raw.get(1..9)?.try_into().ok()?)),
        1 => Some(17 + u64::from_le_bytes(raw.get(1..9)?.try_into().ok()?)),
        _ => None,
    }
}

/// Indexes bigger than this are not read (a big game's is a few MB).
pub const INDEX_MAX: usize = 64 << 20;

/// The members an unpacked .xp3 index (`index`) names: "File" chunks, each with "info" (name)
/// and "segm" (where its bytes are). Anything else, or a malformed one, is passed over.
pub fn xp3_index(index: &[u8]) -> Vec<Member> {
    let mut out = Vec::new();
    for (tag, body) in xp3_chunks(index) {
        if tag != b"File" {
            continue;
        }
        let (mut name, mut segments) = (None, Vec::new());
        for (t, b) in xp3_chunks(body) {
            match t {
                b"info" if b.len() >= 22 => {
                    let n = u16::from_le_bytes([b[20], b[21]]) as usize;
                    let units: Vec<u16> = b[22..].chunks_exact(2).take(n).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                    if units.len() == n {
                        name = Some(String::from_utf16_lossy(&units));
                    }
                }
                b"segm" => {
                    for s in b.chunks_exact(28) {
                        let f = u32::from_le_bytes(s[0..4].try_into().unwrap());
                        let start = u64::from_le_bytes(s[4..12].try_into().unwrap());
                        let packed = u64::from_le_bytes(s[20..28].try_into().unwrap());
                        segments.push((start, packed, f & 7 == 1));
                    }
                }
                _ => {}
            }
        }
        if let Some(name) = name.filter(|_| !segments.is_empty()) {
            out.push(Member { name, segments });
        }
    }
    out
}

/// An xp3 index's chunks: 4-byte tag, u64 length, body.
fn xp3_chunks(b: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let tag = b.get(at..at + 4)?;
        let n = u64::from_le_bytes(b.get(at + 4..at + 12)?.try_into().ok()?);
        let start = at + 12;
        let end = start.checked_add(usize::try_from(n).ok()?)?;
        let body = b.get(start..end)?;
        at = end;
        Some((tag, body))
    })
}

/// A member's bytes from its segments' bytes as they are in the archive (`parts`, in order):
/// zlib'd ones unpacked. None when one does not unpack.
pub fn member_bytes(parts: &[(&[u8], bool)]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for (b, z) in parts {
        if *z {
            out.extend(miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(b, SCRIPT_MAX).ok()?);
        } else {
            out.extend_from_slice(b);
        }
        if out.len() > SCRIPT_MAX {
            return None;
        }
    }
    Some(out)
}

/// Scripts bigger than this are not looked into.
pub const SCRIPT_MAX: usize = 8 << 20;

/// Whether a file named `name` is a script to look into.
pub fn is_script(name: &str) -> bool {
    kind(name) == Kind::Script || matches!(name.rsplit_once('.').map(|x| x.1.to_ascii_lowercase()).as_deref(), Some("asd" | "sli" | "def" | "nss" | "s"))
}

/// A script's text, as characters (`bytes`: the file's): UTF-16 (a BOM), KiriKiri's simple
/// text encryption (`FE FE mode FF FE`, modes 0, 1 and 2), else bytes as they are (Shift_JIS,
/// UTF-8: the names in them are what matters, see [`mentions`]); `wide` says which.
pub fn text(bytes: &[u8]) -> (Vec<u16>, bool) {
    if bytes.len() >= 5 && bytes[0] == 0xfe && bytes[1] == 0xfe && bytes[3] == 0xff && bytes[4] == 0xfe {
        let mode = bytes[2];
        let body = &bytes[5..];
        let units = |b: &[u8]| b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<u16>>();
        return match mode {
            0 => (units(body).into_iter().map(|c| if c >= 0x20 { c ^ (((c & 0xfe) << 8) ^ 1) } else { c }).collect(), true),
            1 => (units(body).into_iter().map(|c| ((c & 0xaaaa) >> 1) | ((c & 0x5555) << 1)).collect(), true),
            2 if body.len() >= 16 => {
                let size = u64::from_le_bytes(body[8..16].try_into().unwrap()) as usize;
                let out = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&body[16..], size.min(SCRIPT_MAX)).unwrap_or_default();
                (units(&out), true)
            }
            _ => (Vec::new(), true),
        };
    }
    if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xfe {
        return (bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect(), true);
    }
    (bytes.iter().map(|&b| b as u16).collect(), false)
}

/// The names a script's text (`text`, `wide` as [`text`] gives them) mentions, in the order it
/// first mentions them, lower case (ASCII), each once: the runs of characters between quotes,
/// spaces, brackets and the like, their last path part. In bytes (not `wide`) only ASCII names:
/// a Shift_JIS character's second byte may be a letter. Not names: shorter than 3, all digits.
pub fn mentions(text: &[u16], wide: bool) -> Vec<String> {
    let delim = |c: u16| c < 0x21 || b"\"'`[](){}<>=,;:*|&+!?#@^~$%".contains(&(c as u8)) && c < 0x80 || (0x3000..=0x303f).contains(&c) || (0xff01..=0xff0f).contains(&c);
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut cur: Vec<u16> = Vec::new();
    let mut flush = |cur: &mut Vec<u16>, out: &mut Vec<String>| {
        let s = String::from_utf16_lossy(cur);
        cur.clear();
        let last = s.rsplit(['/', '\\']).next().unwrap_or("").trim_matches('.').to_ascii_lowercase();
        if last.chars().count() >= 3 && !last.chars().all(|c| c.is_ascii_digit()) && seen.insert(last.clone()) {
            out.push(last);
        }
    };
    let mut i = 0;
    while i < text.len() {
        let c = text[i];
        if !wide && c >= 0x80 {
            // (a Shift_JIS lead byte takes the next with it; UTF-8's are all past 0x80)
            if (0x81..=0x9f).contains(&c) || (0xe0..=0xfc).contains(&c) {
                i += 1;
            }
            flush(&mut cur, &mut out);
        } else if delim(c) || (0xd800..=0xdfff).contains(&c) {
            flush(&mut cur, &mut out);
        } else {
            cur.push(c);
        }
        i += 1;
        if out.len() >= MENTIONS_MAX {
            return out;
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// Names one script gives at most.
pub const MENTIONS_MAX: usize = 4096;

/// A name's keys to look it up by: itself, and without its extension (KiriKiri's scripts name
/// storage without one: "bg01" for bg01.tlg or bg01.png).
pub fn keys(name: &str) -> (String, Option<String>) {
    let n = name.to_ascii_lowercase();
    let stem = n.rsplit_once('.').filter(|(s, _)| !s.is_empty()).map(|(s, _)| s.to_string());
    (n, stem)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;

    fn chunk(tag: &[u8], body: &[u8]) -> Vec<u8> {
        let mut v = tag.to_vec();
        v.extend((body.len() as u64).to_le_bytes());
        v.extend_from_slice(body);
        v
    }
    /// An xp3 index entry of `name` with segments (start, packed, zlib).
    pub(crate) fn file_entry(name: &str, segs: &[(u64, u64, bool)]) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let total: u64 = segs.iter().map(|s| s.1).sum();
        let mut info = 0u32.to_le_bytes().to_vec();
        info.extend(total.to_le_bytes());
        info.extend(total.to_le_bytes());
        info.extend((units.len() as u16).to_le_bytes());
        info.extend(units.iter().flat_map(|u| u.to_le_bytes()));
        let mut segm = Vec::new();
        for &(start, packed, z) in segs {
            segm.extend((z as u32).to_le_bytes());
            segm.extend(start.to_le_bytes());
            segm.extend(packed.to_le_bytes());
            segm.extend(packed.to_le_bytes());
        }
        let mut body = chunk(b"info", &info);
        body.extend(chunk(b"segm", &segm));
        body.extend(chunk(b"adlr", &[0; 4]));
        chunk(b"File", &body)
    }

    #[test]
    fn xp3_index_names_members_and_their_bytes() {
        let mut idx = file_entry("scenario/first.ks", &[(0x100, 50, true)]);
        idx.extend(chunk(b"hnfn", &[1, 2, 3]));
        idx.extend(file_entry("bgimage/bg01.tlg", &[(0x200, 10, false), (0x300, 5, false)]));
        // (malformed: a File without info is left out; a truncated tail ends the list)
        idx.extend(chunk(b"File", &chunk(b"segm", &[0; 28])));
        idx.extend(b"Fil");
        assert_eq!(
            xp3_index(&idx),
            vec![
                Member { name: "scenario/first.ks".into(), segments: vec![(0x100, 50, true)] },
                Member { name: "bgimage/bg01.tlg".into(), segments: vec![(0x200, 10, false), (0x300, 5, false)] },
            ]
        );
        // the header and the index's place, plain and zlib'd
        let mut head = XP3_MAGIC.to_vec();
        head.extend(0x1234u64.to_le_bytes());
        head.resize(32, 0);
        assert_eq!(xp3_index_at(&head), Some(0x1234));
        let mut h23 = XP3_MAGIC.to_vec();
        h23.extend(0x17u64.to_le_bytes());
        h23.resize(0x17, 0);
        h23.push(0x80);
        h23.extend(0u64.to_le_bytes());
        h23.extend(0x9999u64.to_le_bytes());
        assert_eq!(xp3_index_at(&h23), Some(0x9999));
        assert_eq!(xp3_index_at(b"PK\x03\x04 not an xp3 at all......."), None);
        let mut plain = vec![0u8];
        plain.extend((idx.len() as u64).to_le_bytes());
        plain.extend(&idx);
        assert_eq!(xp3_index_len(&plain), Some(plain.len() as u64));
        assert_eq!(xp3_index_bytes(&plain).as_deref(), Some(&idx[..]));
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&idx, 6);
        let mut packed = vec![1u8];
        packed.extend((z.len() as u64).to_le_bytes());
        packed.extend((idx.len() as u64).to_le_bytes());
        packed.extend(&z);
        assert_eq!(xp3_index_len(&packed), Some(packed.len() as u64));
        assert_eq!(xp3_index_bytes(&packed).as_deref(), Some(&idx[..]));
        assert_eq!(xp3_index_bytes(&packed[..packed.len() - 3]), None);
    }

    /// KiriKiri's simple text encryption: modes 0 and 1 are their own inverses; mode 2 is zlib.
    #[test]
    fn kirikiri_text_modes() {
        let s = "[bg storage=\"bg01\"]\r\n@playse storage=se_door 「こんにちは」";
        let units: Vec<u16> = s.encode_utf16().collect();
        let m0: Vec<u16> = units.iter().map(|&c| if c >= 0x20 { c ^ (((c & 0xfe) << 8) ^ 1) } else { c }).collect();
        let m1: Vec<u16> = units.iter().map(|&c| ((c & 0xaaaa) >> 1) | ((c & 0x5555) << 1)).collect();
        let wrap = |mode: u8, body: &[u8]| [&[0xfe, 0xfe, mode, 0xff, 0xfe][..], body].concat();
        let le = |u: &[u16]| u.iter().flat_map(|c| c.to_le_bytes()).collect::<Vec<u8>>();
        assert_eq!(text(&wrap(0, &le(&m0))), (units.clone(), true));
        assert_eq!(text(&wrap(1, &le(&m1))), (units.clone(), true));
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&le(&units), 6);
        let mut b2 = (z.len() as u64).to_le_bytes().to_vec();
        b2.extend((units.len() as u64 * 2).to_le_bytes());
        b2.extend(z);
        assert_eq!(text(&wrap(2, &b2)), (units.clone(), true));
        let bom = [&[0xff, 0xfe][..], &le(&units)].concat();
        assert_eq!(text(&bom), (units.clone(), true));
        for t in [text(&wrap(0, &le(&m0))), text(&bom)] {
            assert_eq!(mentions(&t.0, t.1), ["storage", "bg01", "playse", "se_door", "こんにちは"]);
        }
    }

    #[test]
    fn names_in_scripts() {
        let sjis = b"[bg storage=\"BG01.png\" time=500]\r\n\x83\x5cbg02\x82\xa0 @voice storage=voice/v_0012.ogg\n*label_1|\x8eq [jump target=*x storage=\"scene02.ks\"]";
        let (t, w) = text(sjis);
        assert_eq!(mentions(&t, w), ["storage", "bg01.png", "time", "bg02", "voice", "v_0012.ogg", "label_1", "jump", "target", "scene02.ks"]);
        assert_eq!(keys("BG01.png"), ("bg01.png".into(), Some("bg01".into())));
        assert_eq!(keys("bg01"), ("bg01".into(), None));
        assert_eq!(keys(".hidden"), (".hidden".into(), None));
        assert!(is_script("scenario/first.KS") && is_script("x.tjs") && !is_script("bg01.png"));
    }

    proptest! {
        /// Any bytes: no panic, names of 3 or more characters, each once, none all digits, none
        /// with a delimiter in it.
        #[test]
        fn any_bytes_any_names(b in proptest::collection::vec(any::<u8>(), 0..600)) {
            let (t, w) = text(&b);
            let m = mentions(&t, w);
            let set: std::collections::HashSet<_> = m.iter().collect();
            prop_assert_eq!(set.len(), m.len());
            for n in &m {
                prop_assert!(n.chars().count() >= 3);
                prop_assert!(!n.chars().all(|c| c.is_ascii_digit()));
                prop_assert!(!n.contains(['"', '[', ' ', '=', '/', '\\']));
            }
            let _ = xp3_index(&b);
            let _ = xp3_index_bytes(&b);
            let _ = xp3_index_at(&b);
            let _ = member_bytes(&[(&b, true), (&b, false)]);
        }

        /// A name between delimiters is found, in its place among others.
        #[test]
        fn a_name_written_is_found(names in proptest::collection::vec("[a-z_][a-z0-9_]{2,10}(\\.[a-z]{2,3})?", 1..8)) {
            let s: String = names.iter().map(|n| format!("[x storage=\"{n}\"] ")).collect();
            let (t, w) = text(s.as_bytes());
            let m = mentions(&t, w);
            let mut want = vec!["storage".to_string()];
            for n in &names {
                if !want.contains(n) {
                    want.push(n.clone());
                }
            }
            prop_assert_eq!(m, want);
        }
    }
}
