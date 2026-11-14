//! What a game reads next, foreseen from what it reads now: a script names the pictures, voices
//! and scripts its scene shows and plays ("[bg storage=bg01]", "@voice v_0012"), so when the game
//! reads a script, those files are fetched onto the device before it gets to them (kura vfs.rs
//! `predict_read`, the page's masuHint). The same way whichever way the game goes: the gallery's
//! script names the gallery's pictures, a route's its scenes, on the game's first run too.
//!
//! Two places use it: kura, on the device, reads a script as the game reads it; masu serve
//! reads every script of a tree once, ahead of any play (crates/masu/src/foresee.rs), into a
//! table ([`table_line`], [`parse_table`]) the page hands kura at the start: a script's names
//! are known before it is read, and so are those of the scripts it goes on to.
//!
//! The scripts read here: loose ones (a tree's .ks, .txt, .scn ... files), and the members of
//! KiriKiri's .xp3 archives (half the store's games; their index names each member and where its
//! bytes lie: [`xp3_index`]) as they are in the archive or zlib'd, as text plain, UTF-16, or in
//! KiriKiri's own simple text encryption (`FE FE mode FF FE`: [`text`]). A script a game
//! encrypts its own way (cxdec and the like) reads as noise and names nothing; engines with
//! compiled, packed scripts (Siglus' Scene.pck, BGI) name nothing here either.


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

/// Whether a file named `name` is a script to look into (by its extension, as kura's
/// vfs/hints.rs Kind::Script, and a few more).
pub fn is_script(name: &str) -> bool {
    let Some((stem, e)) = name.rsplit_once('.') else { return false };
    if stem.is_empty() || stem.ends_with(['/', '\\']) {
        return false;
    }
    matches!(
        e.to_ascii_lowercase().as_str(),
        "ks" | "tjs" | "scn" | "txt" | "ss" | "ybn" | "mjo" | "ast" | "nut" | "ain" | "ini" | "csv" | "xml" | "json" | "lua" | "rpy" | "rpyc" | "sob" | "bin_" | "asd" | "sli" | "def" | "nss" | "s"
    )
}

/// The key a script goes by in a table: its name's last part, lower case (as scripts name each
/// other: "second.ks", whatever its folder).
pub fn script_key(name: &str) -> String {
    name.rsplit(['/', '\\']).next().unwrap_or(name).to_ascii_lowercase()
}

/// A table's first line.
pub const TABLE_HEADER: &str = "ippai-foresee 1";

/// A table's line for script `name` and the names it mentions (in order): key, tab, names
/// comma-separated (names with a tab, a comma or a line break in them left out).
pub fn table_line(name: &str, names: &[String]) -> String {
    let ok: Vec<&str> = names.iter().map(String::as_str).filter(|n| !n.contains(['\t', ',', '\n', '\r'])).collect();
    format!("{}\t{}\n", script_key(name).replace(['\t', '\n', '\r'], " "), ok.join(","))
}

/// A table's scripts and their names (a later line for a key adds to it; other lines passed over).
pub fn parse_table(text: &str) -> std::collections::HashMap<String, Vec<String>> {
    let mut out: std::collections::HashMap<String, Vec<String>> = Default::default();
    let mut lines = text.lines();
    if lines.next() != Some(TABLE_HEADER) {
        return out;
    }
    for l in lines {
        let Some((k, v)) = l.split_once('\t') else { continue };
        let e = out.entry(k.to_string()).or_default();
        for n in v.split(',').filter(|n| !n.is_empty()) {
            if !e.iter().any(|x| x == n) {
                e.push(n.to_string());
            }
        }
    }
    out
}

/// What script `key` foresees through `table`: its names, then those of the scripts it names
/// (`depth` levels on, breadth first), each once, `max` at most.
pub fn foreseen(table: &std::collections::HashMap<String, Vec<String>>, stems: &std::collections::HashMap<String, String>, key: &str, depth: usize, max: usize) -> Vec<String> {
    let find = |n: &str| -> Option<String> {
        if table.contains_key(n) {
            return Some(n.to_string());
        }
        let (_, stem) = keys(n);
        stems.get(stem.as_deref().unwrap_or(n)).cloned()
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut done = std::collections::HashSet::from([key.to_string()]);
    let mut level = vec![key.to_string()];
    for _ in 0..=depth {
        let mut next = Vec::new();
        for s in &level {
            for n in table.get(s).into_iter().flatten() {
                if out.len() >= max {
                    return out;
                }
                if seen.insert(n.clone()) {
                    out.push(n.clone());
                }
                if let Some(k) = find(n) {
                    if done.insert(k.clone()) {
                        next.push(k);
                    }
                }
            }
        }
        level = next;
    }
    out
}

/// A table's scripts by their names without extension (`second` for second.ks), for
/// [`foreseen`]: a script names another without one.
pub fn table_stems(table: &std::collections::HashMap<String, Vec<String>>) -> std::collections::HashMap<String, String> {
    table.keys().filter_map(|k| keys(k).1.map(|s| (s, k.clone()))).collect()
}

/// Where each name stands in the table's scripts: name -> (script, position), the first place
/// in each script, for [`following`]. Names in more than `common` scripts are left out (a
/// window frame, a click sound: they say nothing of where the game is).
pub fn table_places(table: &std::collections::HashMap<String, Vec<String>>, common: usize) -> std::collections::HashMap<String, Vec<(String, usize)>> {
    let mut out: std::collections::HashMap<String, Vec<(String, usize)>> = Default::default();
    let mut scripts: Vec<&String> = table.keys().collect();
    scripts.sort();
    for s in scripts {
        for (i, n) in table[s].iter().enumerate() {
            let e = out.entry(n.clone()).or_default();
            if !e.iter().any(|x| &x.0 == s) {
                e.push((s.clone(), i));
            }
        }
    }
    out.retain(|_, v| v.len() <= common);
    out
}

/// What the game reads next after file `name` (a picture, a sound: what a script names), by
/// where it stands in the scripts that name it (`places`, [`table_places`]): the names after it
/// in each, `ahead` of each at most, taken in turn from each script.
pub fn following(table: &std::collections::HashMap<String, Vec<String>>, places: &std::collections::HashMap<String, Vec<(String, usize)>>, name: &str, ahead: usize) -> Vec<String> {
    let (full, stem) = keys(name);
    let Some(at) = places.get(&full).or_else(|| stem.as_ref().and_then(|s| places.get(s))) else { return Vec::new() };
    let lists: Vec<&[String]> = at.iter().filter_map(|(s, i)| table.get(s).map(|l| &l[(i + 1).min(l.len())..(i + 1 + ahead).min(l.len())])).collect();
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for k in 0..ahead {
        for l in &lists {
            if let Some(n) = l.get(k) {
                if seen.insert(n.clone()) {
                    out.push(n.clone());
                }
            }
        }
    }
    out
}

/// A BGI (Ethornell) .arc's members: "PackFile    " (names of 16 bytes) or "BURIKO ARC20" (96),
/// from its first bytes `head` (the index: 16 + count * entry bytes). Each member's segment is
/// (its place in the archive, its size, not zlib'd).
pub fn bgi_index(head: &[u8]) -> Vec<Member> {
    let (name_len, entry) = match head.get(..12) {
        Some(b"PackFile    ") => (16, 32),
        Some(b"BURIKO ARC20") => (96, 128),
        _ => return Vec::new(),
    };
    let Some(n) = head.get(12..16).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize) else { return Vec::new() };
    let base = 16 + n.saturating_mul(entry);
    let mut out = Vec::new();
    for k in 0..n.min(1 << 20) {
        let Some(e) = head.get(16 + k * entry..16 + (k + 1) * entry) else { break };
        let raw = &e[..name_len];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(name_len);
        // (names are Shift_JIS; the ASCII ones are what scripts name)
        let name: String = raw[..end].iter().map(|&b| if b < 0x80 { b as char } else { '_' }).collect();
        let off = u32::from_le_bytes(e[name_len..name_len + 4].try_into().unwrap()) as u64;
        let size = u32::from_le_bytes(e[name_len + 4..name_len + 8].try_into().unwrap()) as u64;
        if !name.is_empty() {
            out.push(Member { name, segments: vec![(base as u64 + off, size, false)] });
        }
    }
    out
}

/// The bytes a BGI .arc's index takes (from its first 16).
pub fn bgi_index_len(head: &[u8]) -> Option<u64> {
    let entry = match head.get(..12)? {
        b"PackFile    " => 32,
        b"BURIKO ARC20" => 128,
        _ => return None,
    };
    Some(16 + u32::from_le_bytes(head.get(12..16)?.try_into().ok()?) as u64 * entry)
}

/// Whether a member's bytes are a BGI compiled script ("BurikoCompiledScriptVer1.00").
pub fn is_bgi_script(head: &[u8]) -> bool {
    head.starts_with(b"BurikoCompiledScriptVer1.00")
}

/// A Siglus scene's strings (`scene`: one scene of Scene.pck, decrypted and unpacked: header
/// i32s, the string index (offset, length in UTF-16 units) at 12, its count at 16, the strings
/// at 20), in order; string k XORed with 28807 * k when `xor` (as most games keep them); a
/// malformed one gives what it has.
pub fn siglus_strings(scene: &[u8], xor: bool) -> Vec<String> {
    let rd = |o: usize| scene.get(o..o + 4).map(|b| i32::from_le_bytes(b.try_into().unwrap()));
    let (Some(idx), Some(cnt), Some(list)) = (rd(12), rd(16), rd(20)) else { return Vec::new() };
    if idx < 0 || cnt < 0 || list < 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for k in 0..(cnt as usize).min(1 << 20) {
        let (Some(o), Some(n)) = (rd(idx as usize + k * 8), rd(idx as usize + k * 8 + 4)) else { break };
        if o < 0 || n < 0 {
            continue;
        }
        let start = list as usize + o as usize * 2;
        let Some(b) = scene.get(start..start + n as usize * 2) else { continue };
        let key = if xor { 28807u32.wrapping_mul(k as u32) as u16 } else { 0 };
        out.push(String::from_utf16_lossy(&b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]) ^ key).collect::<Vec<u16>>()));
    }
    out
}

/// The names in a list of strings (each one's [`mentions`]), in order, each once.
pub fn names_in_strings(strings: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for s in strings {
        let units: Vec<u16> = s.encode_utf16().collect();
        for n in mentions(&units, true) {
            if seen.insert(n.clone()) {
                out.push(n);
            }
        }
    }
    out
}

/// An .xp3 index chunk (tag, length, body).
pub fn xp3_chunk(tag: &[u8], body: &[u8]) -> Vec<u8> {
    let mut v = tag.to_vec();
    v.extend((body.len() as u64).to_le_bytes());
    v.extend_from_slice(body);
    v
}

/// An .xp3 index entry of `name` with segments (start, packed, zlib), as KiriKiri writes them
/// (for tests here and in kura and masu).
pub fn xp3_entry(name: &str, segs: &[(u64, u64, bool)]) -> Vec<u8> {
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
    let mut body = xp3_chunk(b"info", &info);
    body.extend(xp3_chunk(b"segm", &segm));
    body.extend(xp3_chunk(b"adlr", &[0; 4]));
    xp3_chunk(b"File", &body)
}

/// A whole .xp3 of `members` (name, bytes, zlib'd), its index zlib'd at the end (tests).
pub fn xp3_archive(members: &[(&str, &[u8], bool)]) -> Vec<u8> {
    let mut arc = XP3_MAGIC.to_vec();
    arc.extend(0u64.to_le_bytes());
    arc.resize(64, 0);
    let mut idx = Vec::new();
    for (name, body, z) in members {
        let data = if *z { miniz_oxide::deflate::compress_to_vec_zlib(body, 6) } else { body.to_vec() };
        idx.extend(xp3_entry(name, &[(arc.len() as u64, data.len() as u64, *z)]));
        arc.extend_from_slice(&data);
        arc.resize(arc.len().div_ceil(64) * 64, 0);
    }
    let at = arc.len() as u64;
    arc[11..19].copy_from_slice(&at.to_le_bytes());
    let zi = miniz_oxide::deflate::compress_to_vec_zlib(&idx, 6);
    arc.push(1);
    arc.extend((zi.len() as u64).to_le_bytes());
    arc.extend((idx.len() as u64).to_le_bytes());
    arc.extend(&zi);
    arc
}

/// The names a script's bytes (`bytes`, as the file or member holds them unpacked) mention.
pub fn names_in(bytes: &[u8]) -> Vec<String> {
    let (chars, wide) = text(bytes);
    mentions(&chars, wide)
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
mod tests {
    use super::*;
    use proptest::prelude::*;

    use xp3_chunk as chunk;
    use xp3_entry as file_entry;

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

    /// A table: lines back as they went, a later line adding; what a script foresees: its names,
    /// then those of the scripts it names (with or without the extension), breadth first.
    #[test]
    fn tables_and_what_they_foresee() {
        let v = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let mut text = String::from(TABLE_HEADER);
        text.push('\n');
        text += &table_line("scenario/First.ks", &v(&["bg01", "v002", "second.ks", "a,b", "gallery"]));
        text += &table_line("second.ks", &v(&["bg02", "third"]));
        text += &table_line("third.ks", &v(&["bg03", "first.ks"]));
        text += &table_line("gallery.ks", &v(&["cg01"]));
        text += &table_line("second.ks", &v(&["se01", "bg02"]));
        let t = parse_table(&text);
        assert_eq!(t["first.ks"], v(&["bg01", "v002", "second.ks", "gallery"]), "a name with a comma left out");
        assert_eq!(t["second.ks"], v(&["bg02", "third", "se01"]));
        let stems = table_stems(&t);
        assert_eq!(foreseen(&t, &stems, "first.ks", 0, 100), v(&["bg01", "v002", "second.ks", "gallery"]));
        assert_eq!(foreseen(&t, &stems, "first.ks", 1, 100), v(&["bg01", "v002", "second.ks", "gallery", "bg02", "third", "se01", "cg01"]));
        // (a loop back to first.ks: once)
        assert_eq!(foreseen(&t, &stems, "first.ks", 5, 100), v(&["bg01", "v002", "second.ks", "gallery", "bg02", "third", "se01", "cg01", "bg03", "first.ks"]));
        assert_eq!(foreseen(&t, &stems, "first.ks", 5, 3).len(), 3);
        assert!(foreseen(&t, &stems, "nothing.ks", 3, 100).is_empty());
        assert!(parse_table("not a table\nx\ty").is_empty());
        // (an archive written and read back: its members, their bytes)
        let a = xp3_archive(&[("scenario/a.ks", b"[bg storage=bg09]", true), ("bg09.png", b"png", false)]);
        let at = xp3_index_at(&a).unwrap() as usize;
        let ms = xp3_index(&xp3_index_bytes(&a[at..]).unwrap());
        assert_eq!(ms.len(), 2);
        let (s0, n0, z0) = ms[0].segments[0];
        let body = member_bytes(&[(&a[s0 as usize..(s0 + n0) as usize], z0)]).unwrap();
        assert_eq!(names_in(&body), v(&["storage", "bg09"]));
    }

    /// A picture read mid-scene: what the scenes that name it name after it, in turn; names
    /// in too many scenes say nothing.
    #[test]
    fn what_comes_after_a_file_in_its_scenes() {
        let v = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let mut t = std::collections::HashMap::new();
        t.insert("a.ks".to_string(), v(&["frame", "bg01", "ch01", "ch02", "bg02"]));
        t.insert("b.ks".to_string(), v(&["frame", "bg09", "ch01", "ch07"]));
        t.insert("c.ks".to_string(), v(&["frame", "ev01"]));
        let p = table_places(&t, 2);
        assert!(!p.contains_key("frame"), "in all three");
        assert_eq!(following(&t, &p, "bg01.png", 2), v(&["ch01", "ch02"]));
        assert_eq!(following(&t, &p, "ch01", 3), v(&["ch02", "ch07", "bg02"]));
        assert_eq!(following(&t, &p, "bg02", 3), Vec::<String>::new());
        assert!(following(&t, &p, "nothing", 3).is_empty());
    }

    #[test]
    fn bgi_archives_and_scripts() {
        for (magic, name_len, entry) in [(&b"PackFile    "[..], 16usize, 32usize), (b"BURIKO ARC20", 96, 128)] {
            let mut a = magic.to_vec();
            a.extend(2u32.to_le_bytes());
            for (name, off, size) in [("01_start", 0u32, 40u32), ("bg01a", 40, 7)] {
                let mut e = vec![0u8; entry];
                e[..name.len()].copy_from_slice(name.as_bytes());
                e[name_len..name_len + 4].copy_from_slice(&off.to_le_bytes());
                e[name_len + 4..name_len + 8].copy_from_slice(&size.to_le_bytes());
                a.extend(e);
            }
            assert_eq!(bgi_index_len(&a), Some(a.len() as u64));
            let base = a.len() as u64;
            let ms = bgi_index(&a);
            assert_eq!(ms, vec![Member { name: "01_start".into(), segments: vec![(base, 40, false)] }, Member { name: "bg01a".into(), segments: vec![(base + 40, 7, false)] }]);
            // (a truncated index: what it has)
            assert_eq!(bgi_index(&a[..a.len() - 1]).len(), 1);
        }
        assert!(bgi_index(b"XP3\r\n").is_empty() && bgi_index_len(b"nope").is_none());
        let script = b"BurikoCompiledScriptVer1.00\0\x10\0\0\0....bg01a\0ch_03b\0\x82\xa0\0";
        assert!(is_bgi_script(script));
        assert!(names_in(script).ends_with(&["bg01a".to_string(), "ch_03b".to_string()]));
    }

    /// A Siglus scene's string table, read back.
    #[test]
    fn siglus_scene_strings() {
        let strs = ["bg_01a", "\u{3053}\u{3093}", "se_door"];
        let mut s = vec![0u8; 32];
        let units: Vec<Vec<u16>> = strs.iter().map(|x| x.encode_utf16().collect()).collect();
        let idx = 32;
        let list = idx + 8 * strs.len();
        s[12..16].copy_from_slice(&(idx as i32).to_le_bytes());
        s[16..20].copy_from_slice(&(strs.len() as i32).to_le_bytes());
        s[20..24].copy_from_slice(&(list as i32).to_le_bytes());
        let mut at = 0i32;
        for u in &units {
            s.extend(at.to_le_bytes());
            s.extend((u.len() as i32).to_le_bytes());
            at += u.len() as i32;
        }
        let plain = s.clone();
        for u in &units {
            s.extend(u.iter().flat_map(|c| c.to_le_bytes()));
        }
        let mut x = plain;
        for (k, u) in units.iter().enumerate() {
            x.extend(u.iter().flat_map(|c| (c ^ 28807u32.wrapping_mul(k as u32) as u16).to_le_bytes()));
        }
        assert_eq!(siglus_strings(&s, false), strs);
        assert_eq!(siglus_strings(&x, true), strs);
        assert_eq!(names_in_strings(&siglus_strings(&s, false)), ["bg_01a", "se_door"]);
        assert!(siglus_strings(&s[..20], false).is_empty());
        assert_eq!(siglus_strings(&s[..s.len() - 2], false).len(), 2, "the last one cut: left out");
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
            let _ = bgi_index(&b);
            let _ = bgi_index_len(&b);
            let _ = siglus_strings(&b, true);
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
