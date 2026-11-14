//! `masu ttc <out.ttc> <a.ttf> <b.ttf>...`: one TrueType collection from several fonts that
//! share most of their data, as Windows ships MS Gothic (msgothic.ttc: MS Gothic, MS UI Gothic,
//! MS PGothic). Identical tables are stored once, and so are glyphs: one `glyf` holds per
//! glyph id the first font's glyph and the others' where theirs differ, each font's `loca`
//! pointing into it. Hinting instructions are dropped (sake draws unhinted; the MS-named
//! fonts' glyphs differ only there). The page downloads the fonts before a game draws text.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

struct Font {
    version: u32,
    /// (tag, data), in the file's order
    tables: Vec<([u8; 4], Vec<u8>)>,
}

// (by `get`: a font cut short or damaged is an error, not a panic; review b01)
fn be16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(o..o.checked_add(2)?)?.try_into().ok()?))
}
fn be32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(o..o.checked_add(4)?)?.try_into().ok()?))
}

fn parse(p: &Path) -> Result<Font> {
    let d = std::fs::read(p).with_context(|| format!("{}", p.display()))?;
    parse_bytes(&d).with_context(|| format!("{}", p.display()))
}

fn parse_bytes(d: &[u8]) -> Result<Font> {
    if d.len() < 12 || &d[..4] == b"ttcf" {
        bail!("not a single TrueType font");
    }
    let cut = || anyhow::anyhow!("its table directory is cut short");
    let n = be16(d, 4).ok_or_else(cut)? as usize;
    // (a directory's fields are 16-bit: n x 16 must fit; real fonts have a few dozen tables)
    if n == 0 || n > 4095 {
        bail!("{n} tables");
    }
    let mut tables = Vec::new();
    for i in 0..n {
        let r = 12 + 16 * i;
        let (off, len) = (be32(d, r + 8).ok_or_else(cut)? as usize, be32(d, r + 12).ok_or_else(cut)? as usize);
        let data = off.checked_add(len).and_then(|end| d.get(off..end)).with_context(|| format!("table {i} out of the file"))?;
        let tag: [u8; 4] = d.get(r..r + 4).ok_or_else(cut)?.try_into()?;
        tables.push((tag, data.to_vec()));
    }
    Ok(Font { version: be32(d, 0).ok_or_else(cut)?, tables })
}

fn table<'a>(f: &'a Font, tag: &[u8; 4]) -> Option<&'a Vec<u8>> {
    f.tables.iter().find(|t| &t.0 == tag).map(|t| &t.1)
}

fn checksum(d: &[u8]) -> u32 {
    d.chunks(4).fold(0u32, |s, c| {
        let mut w = [0u8; 4];
        w[..c.len()].copy_from_slice(c);
        s.wrapping_add(u32::from_be_bytes(w))
    })
}

/// The glyph data of a font, one slice per glyph (`loca` short or long).
fn glyphs(f: &Font) -> Result<Vec<&[u8]>> {
    let (head, glyf, loca) = (
        table(f, b"head").context("no head")?,
        table(f, b"glyf").context("no glyf")?,
        table(f, b"loca").context("no loca")?,
    );
    if head.len() < 54 {
        bail!("head cut short");
    }
    let offs: Vec<usize> = if be16(head, 50).context("head cut short")? == 1 {
        loca.chunks_exact(4).map(|c| u32::from_be_bytes(c.try_into().unwrap()) as usize).collect()
    } else {
        loca.chunks_exact(2).map(|c| u16::from_be_bytes(c.try_into().unwrap()) as usize * 2).collect()
    };
    offs.windows(2).map(|w| glyf.get(w[0]..w[1].max(w[0])).context("loca past glyf")).collect()
}

/// A glyph without its hinting instructions (sake draws outlines unhinted, with ab_glyph):
/// the MS-named fonts' glyphs differ only there (their `cvt` tables differ).
fn unhinted(g: &[u8]) -> Option<Vec<u8>> {
    if g.len() < 10 {
        return Some(g.to_vec());
    }
    let nc = i16::from_be_bytes([g[0], g[1]]);
    if nc >= 0 {
        // header, endPtsOfContours, instructionLength, instructions, the rest
        let at = 10 + 2 * nc as usize;
        let n = be16(g, at)? as usize;
        let mut out = g.get(..at)?.to_vec();
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(g.get(at + 2 + n..)?);
        return Some(out);
    }
    // composite: component records, then instructions if a record says WE_HAVE_INSTRUCTIONS
    let mut at = 10;
    let mut instr = false;
    loop {
        let flags = be16(g, at)?;
        at += 4 + if flags & 1 != 0 { 4 } else { 2 };
        at += if flags & 8 != 0 { 2 } else if flags & 0x40 != 0 { 4 } else if flags & 0x80 != 0 { 8 } else { 0 };
        instr |= flags & 0x100 != 0;
        if flags & 0x20 == 0 {
            break;
        }
        if at + 4 > g.len() {
            return None;
        }
    }
    let mut out = g.get(..at)?.to_vec();
    if instr {
        // (the flag stays: no instructions follow, length 0)
        out.extend_from_slice(&[0, 0]);
    }
    Some(out)
}

/// One `glyf` for all the fonts (the same glyph count): per glyph id the first font's glyph,
/// then the other fonts' when theirs differ; returns it and each font's long `loca`. A
/// glyph's range then runs on over the variants after it, which parsers ignore (as padding).
fn merge_glyphs(fonts: &[Font]) -> Result<(Vec<u8>, Vec<Vec<u8>>)> {
    let all: Vec<Vec<Vec<u8>>> = fonts
        .iter()
        .map(|f| glyphs(f)?.into_iter().map(|g| unhinted(g).context("a glyph cut short")).collect())
        .collect::<Result<_>>()?;
    let n = all[0].len();
    if all.iter().any(|g| g.len() != n) {
        bail!("the fonts have different glyph counts");
    }
    let mut glyf = Vec::new();
    // where each font's glyph starts (None: empty)
    let mut at = vec![vec![None; n]; fonts.len()];
    for i in 0..n {
        let mut placed: Vec<(&[u8], u32)> = Vec::new();
        for (k, g) in all.iter().enumerate() {
            let bytes = g[i].as_slice();
            if bytes.is_empty() {
                continue;
            }
            let pos = match placed.iter().find(|(b, _)| *b == bytes) {
                Some(&(_, p)) => p,
                None => {
                    let p = glyf.len() as u32;
                    glyf.extend_from_slice(bytes);
                    while glyf.len() % 4 != 0 {
                        glyf.push(0);
                    }
                    placed.push((bytes, p));
                    p
                }
            };
            at[k][i] = Some(pos);
        }
    }
    let locas = at
        .iter()
        .map(|a| {
            // (an empty glyph starts where the next one does: no length)
            let mut offs = vec![glyf.len() as u32; n + 1];
            for i in (0..n).rev() {
                offs[i] = a[i].unwrap_or(offs[i + 1]);
            }
            offs.iter().flat_map(|o| o.to_be_bytes()).collect()
        })
        .collect();
    Ok((glyf, locas))
}

/// Writes the collection of `inputs` to `out`.
pub fn run(out: &Path, inputs: &[PathBuf]) -> Result<()> {
    let fonts: Vec<Font> = inputs.iter().map(|p| parse(p)).collect::<Result<_>>()?;
    let o = build(fonts)?;
    std::fs::write(out, &o)?;
    let was: u64 = inputs.iter().map(|p| std::fs::metadata(p).map_or(0, |m| m.len())).sum();
    eprintln!("{}: {} fonts, {} bytes (the files: {was})", out.display(), inputs.len(), o.len());
    Ok(())
}

/// The collection of the fonts in `files` (each a whole .ttf), in memory (fuzz/fuzz_targets/ttc.rs).
#[cfg_attr(not(test), allow(dead_code))]
pub fn collection(files: &[&[u8]]) -> Result<Vec<u8>> {
    build(files.iter().map(|d| parse_bytes(d)).collect::<Result<_>>()?)
}

fn build(mut fonts: Vec<Font>) -> Result<Vec<u8>> {
    if fonts.is_empty() {
        bail!("no fonts");
    }
    let (glyf, locas) = merge_glyphs(&fonts)?;
    for (f, loca) in fonts.iter_mut().zip(locas) {
        for (tag, data) in f.tables.iter_mut() {
            match &*tag {
                b"glyf" => *data = glyf.clone(),
                b"loca" => *data = loca.clone(),
                // long loca; the checksum adjustment of a whole file means nothing in a collection
                b"head" => {
                    // (a font may list a table twice: each head is checked, not only the first; found by fuzz/ttc)
                    if data.len() < 54 {
                        bail!("head cut short");
                    }
                    data[50..52].copy_from_slice(&1u16.to_be_bytes());
                    data[8..12].fill(0);
                }
                _ => {}
            }
        }
    }
    // the header, the fonts' table directories, then each distinct table once
    let dirs: usize = fonts.iter().map(|f| 12 + 16 * f.tables.len()).sum();
    let mut pos = 12 + 4 * fonts.len() + dirs;
    let mut stored: HashMap<&[u8], usize> = HashMap::new();
    let mut order: Vec<&[u8]> = Vec::new();
    for f in &fonts {
        for (_, d) in &f.tables {
            stored.entry(d.as_slice()).or_insert_with(|| {
                let p = pos;
                pos += d.len().div_ceil(4) * 4;
                order.push(d.as_slice());
                p
            });
        }
    }
    let mut o = Vec::with_capacity(pos);
    o.extend_from_slice(b"ttcf");
    o.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    o.extend_from_slice(&(fonts.len() as u32).to_be_bytes());
    let mut dir = 12 + 4 * fonts.len();
    for f in &fonts {
        o.extend_from_slice(&(dir as u32).to_be_bytes());
        dir += 12 + 16 * f.tables.len();
    }
    for f in &fonts {
        let n = f.tables.len() as u16;
        let pow = 1u16 << (15 - n.leading_zeros());
        o.extend_from_slice(&f.version.to_be_bytes());
        for v in [n, pow * 16, pow.trailing_zeros() as u16, n * 16 - pow * 16] {
            o.extend_from_slice(&v.to_be_bytes());
        }
        let mut t: Vec<_> = f.tables.iter().collect();
        t.sort_by_key(|t| t.0);
        for (tag, d) in t {
            o.extend_from_slice(tag);
            o.extend_from_slice(&checksum(d).to_be_bytes());
            o.extend_from_slice(&(stored[d.as_slice()] as u32).to_be_bytes());
            o.extend_from_slice(&(d.len() as u32).to_be_bytes());
        }
    }
    for d in order {
        o.extend_from_slice(d);
        while o.len() % 4 != 0 {
            o.push(0);
        }
    }
    Ok(o)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A TrueType font of `tables` (tag, data), as a file.
    fn font(tables: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut o = 0x0001_0000u32.to_be_bytes().to_vec();
        o.extend_from_slice(&(tables.len() as u16).to_be_bytes());
        o.extend_from_slice(&[0; 6]);
        let mut at = 12 + 16 * tables.len();
        let mut data = Vec::new();
        for (tag, d) in tables {
            o.extend_from_slice(*tag);
            o.extend_from_slice(&checksum(d).to_be_bytes());
            o.extend_from_slice(&(at as u32).to_be_bytes());
            o.extend_from_slice(&(d.len() as u32).to_be_bytes());
            data.extend_from_slice(d);
            while data.len() % 4 != 0 {
                data.push(0);
            }
            at = 12 + 16 * tables.len() + data.len();
        }
        o.extend(data);
        o
    }

    /// A font of two glyphs (one simple glyph with `hint` bytes of instructions, one empty).
    pub(crate) fn sample(hint: &[u8]) -> Vec<u8> {
        let mut g = vec![0, 1, 0, 0, 0, 0, 0, 10, 0, 10, 0, 0];
        g.extend_from_slice(&(hint.len() as u16).to_be_bytes());
        g.extend_from_slice(hint);
        g.extend_from_slice(&[1, 0, 0]);
        while g.len() % 4 != 0 {
            g.push(0);
        }
        let loca: Vec<u8> = [0u16, (g.len() / 2) as u16, (g.len() / 2) as u16].iter().flat_map(|v| v.to_be_bytes()).collect();
        font(&[(b"head", vec![0; 54]), (b"glyf", g), (b"loca", loca), (b"name", b"same in both".to_vec())])
    }

    /// Two fonts that differ in their hinting only: one glyph stored, the shared table once.
    #[test]
    fn collects_two_fonts() {
        let (a, b) = (sample(b"\x01\x02"), sample(b"\x03\x04\x05\x06"));
        let c = collection(&[&a, &b]).unwrap();
        assert_eq!(&c[..4], b"ttcf");
        assert_eq!(be32(&c, 8), Some(2));
        assert!(c.len() < a.len() + b.len());
        assert_eq!(c.windows(12).filter(|w| *w == b"same in both").count(), 1);
    }

    /// Review b01: a font cut short anywhere, or with a damaged directory, is an error.
    #[test]
    fn a_font_cut_short_is_an_error() {
        let a = sample(b"\x01");
        for n in 0..a.len() {
            let _ = collection(&[&a[..n]]);
        }
        // (found by fuzz/ttc: a second head, cut short)
        assert!(collection(&[&font(&[(b"head", vec![0; 54]), (b"glyf", vec![]), (b"loca", vec![0, 0]), (b"head", vec![0; 10])])]).is_err());
        let mut many = a.clone();
        many[4..6].copy_from_slice(&5000u16.to_be_bytes());
        assert!(collection(&[&many]).is_err());
        assert!(collection(&[]).is_err());
    }

    proptest::proptest! {
        /// Any bytes changed in the fonts: an answer or an error, never a panic.
        #[test]
        fn damaged_fonts_never_panic(edits in proptest::collection::vec((0usize..400, proptest::prelude::any::<u8>()), 0..12), cut in 0usize..400, two in proptest::bool::ANY) {
            let mut a = sample(b"\x01\x02\x03");
            for (at, v) in edits {
                if let Some(b) = a.get_mut(at) {
                    *b = v;
                }
            }
            a.truncate(cut.max(12));
            let b = sample(b"");
            let _ = if two { collection(&[&a, &b]) } else { collection(&[&a]) };
        }
    }
}
