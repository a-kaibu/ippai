//! `ippai ttc <out.ttc> <a.ttf> <b.ttf>...`: one TrueType collection from several fonts that
//! share most of their data, as Windows ships MS Gothic (msgothic.ttc: MS Gothic, MS PGothic,
//! MS UI Gothic). Identical tables are stored once, and so are glyphs: one `glyf` holds per
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

fn be16(d: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([d[o], d[o + 1]])
}
fn be32(d: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(d[o..o + 4].try_into().unwrap())
}

fn parse(p: &Path) -> Result<Font> {
    let d = std::fs::read(p).with_context(|| format!("{}", p.display()))?;
    if d.len() < 12 || &d[..4] == b"ttcf" {
        bail!("{}: not a single TrueType font", p.display());
    }
    let n = be16(&d, 4) as usize;
    let mut tables = Vec::new();
    for i in 0..n {
        let r = 12 + 16 * i;
        let (off, len) = (be32(&d, r + 8) as usize, be32(&d, r + 12) as usize);
        let data = d.get(off..off + len).with_context(|| format!("{}: table {i} out of the file", p.display()))?;
        tables.push((d[r..r + 4].try_into().unwrap(), data.to_vec()));
    }
    Ok(Font { version: be32(&d, 0), tables })
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
    let offs: Vec<usize> = if be16(head, 50) == 1 {
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
        let n = be16(g, at) as usize;
        let mut out = g[..at].to_vec();
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(g.get(at + 2 + n..)?);
        return Some(out);
    }
    // composite: component records, then instructions if a record says WE_HAVE_INSTRUCTIONS
    let mut at = 10;
    let mut instr = false;
    loop {
        let flags = be16(g, at);
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
    let mut fonts: Vec<Font> = inputs.iter().map(|p| parse(p)).collect::<Result<_>>()?;
    let (glyf, locas) = merge_glyphs(&fonts)?;
    for (f, loca) in fonts.iter_mut().zip(locas) {
        for (tag, data) in f.tables.iter_mut() {
            match &*tag {
                b"glyf" => *data = glyf.clone(),
                b"loca" => *data = loca.clone(),
                // long loca; the checksum adjustment of a whole file means nothing in a collection
                b"head" => {
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
    std::fs::write(out, &o)?;
    let was: u64 = inputs.iter().map(|p| std::fs::metadata(p).map_or(0, |m| m.len())).sum();
    eprintln!("{}: {} fonts, {} bytes (the files: {was})", out.display(), fonts.len(), o.len());
    Ok(())
}
