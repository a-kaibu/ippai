//! cabset.rs on cabinet sets written here the way Microsoft's cabinet tools split them: one
//! folder going on through the volumes, its blocks cut at a volume's end or not, files that cross
//! listed in each volume (0xFFFE, 0xFFFF, 0xFFFD), a folder of its own in the first volume.
//! Properties: any files, block sizes, compression and cuts read back byte for byte, in any
//! order; damaged volumes are an error, never a panic.
use super::*;
use proptest::prelude::*;
use std::io::Cursor;

/// The volumes of a set: `files` in one folder spanning the volumes (`mszip`: each block
/// deflated on its own), cut before the blocks in `cuts` (a cut block when `split`), plus a
/// stored folder of its own with "lead.txt" in the first volume when `lead`.
fn build(files: &[(String, Vec<u8>)], block: usize, mszip: bool, cuts: &[usize], split: bool, lead: bool) -> Vec<Vec<u8>> {
    let all: Vec<u8> = files.iter().flat_map(|f| f.1.clone()).collect();
    let mut offs = Vec::new();
    let mut o = 0u32;
    for f in files {
        offs.push(o);
        o += f.1.len() as u32;
    }
    let chunks: Vec<&[u8]> = if all.is_empty() { vec![&[][..]] } else { all.chunks(block).collect() };
    let blocks: Vec<(Vec<u8>, usize)> = chunks
        .iter()
        .map(|c| {
            let data = if mszip {
                let mut d = b"CK".to_vec();
                d.extend(miniz_oxide::deflate::compress_to_vec(c, 6));
                d
            } else {
                c.to_vec()
            };
            (data, c.len())
        })
        .collect();
    let n = blocks.len();
    let mut cuts: Vec<usize> = cuts.iter().map(|c| 1 + c % n.max(1)).filter(|&c| c < n).collect();
    cuts.sort();
    cuts.dedup();
    // each volume's blocks: (bytes, cbUncomp) and its uncompressed span
    let mut vols: Vec<(Vec<(Vec<u8>, usize)>, u64, u64)> = Vec::new();
    let mut starts = vec![0];
    starts.extend(&cuts);
    let ustart = |b: usize| (b * block) as u64;
    for (k, &s) in starts.iter().enumerate() {
        let e = starts.get(k + 1).copied().unwrap_or(n);
        let mut bl: Vec<(Vec<u8>, usize)> = Vec::new();
        let mut lo = ustart(s);
        if k > 0 && split {
            // the rest of the block cut at the previous volume's end
            let (d, un) = &blocks[s - 1];
            bl.push((d[d.len() / 2..].to_vec(), *un));
            lo = ustart(s - 1);
        }
        for b in s..e {
            bl.push(blocks[b].clone());
        }
        if k + 1 < starts.len() && split {
            let (d, _) = bl.pop().unwrap();
            bl.push((d[..d.len() / 2].to_vec(), 0));
        }
        vols.push((bl, lo, (ustart(e)).min(all.len() as u64).max(lo)));
    }
    let nv = vols.len();
    let mut out = Vec::new();
    for (k, (bl, lo, hi)) in vols.iter().enumerate() {
        let lead_here = lead && k == 0;
        let span_index = lead_here as u16;
        let mut entries: Vec<(u32, u32, u16, Vec<u8>)> = Vec::new();
        if lead_here {
            entries.push((5, 0, 0, b"lead.txt".to_vec()));
        }
        for (i, f) in files.iter().enumerate() {
            let (a, b) = (offs[i] as u64, offs[i] as u64 + f.1.len() as u64);
            // the volumes holding some of it (a file of no bytes: where it starts)
            let touches = |v: usize| {
                let (l, h) = (vols[v].1, vols[v].2);
                if a == b { v == (0..nv).find(|&u| a < vols[u].2).unwrap_or(nv - 1) } else { a < h && b > l }
            };
            if !touches(k) {
                continue;
            }
            let from_prev = k > 0 && touches(k - 1);
            let to_next = k + 1 < nv && touches(k + 1);
            let idx = match (from_prev, to_next) {
                (true, true) => 0xffff,
                (true, false) => 0xfffd,
                (false, true) => 0xfffe,
                (false, false) => if k == 0 { span_index } else { 0 },
            };
            entries.push((f.1.len() as u32, offs[i], idx, f.0.as_bytes().to_vec()));
        }
        let _ = (lo, hi);
        let mut names: Vec<u8> = Vec::new();
        let mut flags = 0u16;
        if k > 0 {
            flags |= 1;
            names.extend(format!("v{}.cab\0D\0", k - 1).as_bytes());
        }
        if k + 1 < nv {
            flags |= 2;
            names.extend(format!("v{}.cab\0D\0", k + 1).as_bytes());
        }
        let nfold = 1 + lead_here as usize;
        let folders_at = 36 + names.len();
        let files_at = folders_at + 8 * nfold;
        let mut ftab = Vec::new();
        for (size, off, idx, name) in &entries {
            ftab.extend(size.to_le_bytes());
            ftab.extend(off.to_le_bytes());
            ftab.extend(idx.to_le_bytes());
            ftab.extend([0u8; 6]);
            ftab.extend(name);
            ftab.push(0);
        }
        let mut data = Vec::new();
        let data_at = files_at + ftab.len();
        let mut fold = Vec::new();
        if lead_here {
            fold.extend((data_at as u32).to_le_bytes());
            fold.extend(1u16.to_le_bytes());
            fold.extend(0u16.to_le_bytes());
            data.extend([0u8; 4]);
            data.extend(5u16.to_le_bytes());
            data.extend(5u16.to_le_bytes());
            data.extend(b"lead!");
        }
        fold.extend(((data_at + data.len()) as u32).to_le_bytes());
        fold.extend((bl.len() as u16).to_le_bytes());
        fold.extend((mszip as u16).to_le_bytes());
        for (d, un) in bl {
            data.extend([0u8; 4]);
            data.extend((d.len() as u16).to_le_bytes());
            data.extend((*un as u16).to_le_bytes());
            data.extend(d);
        }
        let total = data_at + data.len();
        let mut v = b"MSCF".to_vec();
        v.extend(0u32.to_le_bytes());
        v.extend((total as u32).to_le_bytes());
        v.extend(0u32.to_le_bytes());
        v.extend((files_at as u32).to_le_bytes());
        v.extend(0u32.to_le_bytes());
        v.extend([3, 1]);
        v.extend((nfold as u16).to_le_bytes());
        v.extend((entries.len() as u16).to_le_bytes());
        v.extend(flags.to_le_bytes());
        v.extend(1111u16.to_le_bytes());
        v.extend((k as u16).to_le_bytes());
        v.extend(names);
        v.extend(fold);
        v.extend(ftab);
        v.extend(data);
        assert_eq!(v.len(), total);
        out.push(v);
    }
    out
}

fn read_all(vols: Vec<Vec<u8>>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, u32> {
    let mut s = Set::new(vols.into_iter().map(Cursor::new).collect())?;
    let mut got = Vec::new();
    for i in 0..s.files.len() {
        let mut w = Vec::new();
        s.read_file(i, &mut w)?;
        got.push((s.files[i].name.clone(), w));
    }
    Ok(got)
}

fn bytes(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) ^ (i >> 7) as u8).collect()
}

#[test]
fn two_volumes_like_tenkyokuhime() {
    // Data1.cab: a folder of its own, then the big folder going on into Data11.cab, its last
    // block cut; a file that crosses (0xFFFE in the first, 0xFFFD in the second)
    let files: Vec<(String, Vec<u8>)> = vec![("a.dat".into(), bytes(70_000, 1)), ("b.dat".into(), bytes(90_000, 2)), ("c.dat".into(), bytes(20_000, 3))];
    for mszip in [false, true] {
        for split in [false, true] {
            let vols = build(&files, 0x8000, mszip, &[2], split, true);
            assert_eq!(vols.len(), 2);
            let got = read_all(vols).unwrap();
            assert_eq!(got.len(), 4, "each file once, mszip {mszip} split {split}");
            assert_eq!(got[0], (b"lead.txt".to_vec(), b"lead!".to_vec()));
            for (name, data) in &files {
                assert!(got.iter().any(|g| g.0 == name.as_bytes() && g.1 == *data), "{name} mszip {mszip} split {split}");
            }
        }
    }
}

#[test]
fn second_volume_alone_skips_what_came_before() {
    let files: Vec<(String, Vec<u8>)> = vec![("a".into(), bytes(50_000, 4)), ("b".into(), bytes(50_000, 5))];
    let vols = build(&files, 0x8000, false, &[1], false, false);
    let s = Set::new(vec![Cursor::new(vols[1].clone())]).unwrap();
    assert!(s.files.iter().all(|f| f.name != b"a"), "a started in the first volume");
}

#[test]
fn a_missing_next_volume_is_an_error() {
    let files: Vec<(String, Vec<u8>)> = vec![("a".into(), bytes(100_000, 6))];
    let vols = build(&files, 0x8000, true, &[1], true, false);
    let mut s = Set::new(vec![Cursor::new(vols[0].clone())]).unwrap();
    let mut w = Vec::new();
    assert_eq!(s.read_file(0, &mut w), Err(FDIERROR_CORRUPT_CABINET));
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]
    /// Any files, block size, compression and cuts: every file reads back whole, in any order.
    #[test]
    fn sets_read_back(sizes in proptest::collection::vec(0usize..40_000, 1..8), block in prop_oneof![Just(0x8000usize), 512usize..0x8000],
                      mszip: bool, cuts in proptest::collection::vec(any::<usize>(), 0..5), split: bool, lead: bool, order in any::<u64>()) {
        let files: Vec<(String, Vec<u8>)> = sizes.iter().enumerate().map(|(i, &n)| (format!("f{i}"), bytes(n, i as u8))).collect();
        let vols = build(&files, block, mszip, &cuts, split, lead);
        let mut s = Set::new(vols.into_iter().map(Cursor::new).collect()).unwrap();
        prop_assert_eq!(s.files.len(), files.len() + lead as usize);
        let mut idx: Vec<usize> = (0..s.files.len()).collect();
        // (a shuffled order: files behind the stream restart the folder)
        let mut r = order;
        for i in (1..idx.len()).rev() {
            r = r.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            idx.swap(i, (r >> 33) as usize % (i + 1));
        }
        for i in idx {
            let mut w = Vec::new();
            s.read_file(i, &mut w).unwrap();
            let name = s.files[i].name.clone();
            let want = if name == b"lead.txt" { b"lead!".to_vec() } else { files.iter().find(|f| f.0.as_bytes() == name).unwrap().1.clone() };
            prop_assert_eq!(w, want);
        }
    }

    /// Damaged volumes: an error or some bytes, never a panic.
    #[test]
    fn damaged_sets(sizes in proptest::collection::vec(0usize..20_000, 1..4), mszip: bool, cuts in proptest::collection::vec(any::<usize>(), 1..3),
                    hits in proptest::collection::vec((any::<usize>(), any::<usize>(), any::<u8>()), 1..6), cut in any::<Option<(usize, usize)>>()) {
        let files: Vec<(String, Vec<u8>)> = sizes.iter().enumerate().map(|(i, &n)| (format!("f{i}"), bytes(n, i as u8))).collect();
        let mut vols = build(&files, 4096, mszip, &cuts, true, true);
        for (v, at, b) in hits {
            let n = vols.len();
            let vol = &mut vols[v % n];
            let l = vol.len();
            vol[at % l] = b;
        }
        if let Some((v, at)) = cut {
            let n = vols.len();
            let vol = &mut vols[v % n];
            let l = vol.len();
            vol.truncate(at % l);
        }
        if let Ok(mut s) = Set::new(vols.into_iter().map(Cursor::new).collect()) {
            for i in 0..s.files.len().min(16) {
                let mut w = Vec::new();
                let _ = s.read_file(i, &mut w);
            }
        }
    }
}
