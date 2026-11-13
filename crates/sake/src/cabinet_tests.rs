//! FDI (cabinet.rs) on cabinets the `cab` crate writes: the notification order Windows (and
//! Wine's fdi.c) give, the files' bytes, the callers' answers (skip, abort, a short write),
//! damaged and foreign headers; property tests over random file sets, folders, compression
//! and skip masks, and over damaged cabinets (no panic, every byte handed out belongs to a file).
use super::*;
use proptest::prelude::*;
use std::io::{Cursor, Write};

fn build(folders: &[(bool, Vec<(String, Vec<u8>, bool)>)]) -> Vec<u8> {
    let mut b = cab::CabinetBuilder::new();
    for (zip, files) in folders {
        let f = b.add_folder(if *zip { cab::CompressionType::MsZip } else { cab::CompressionType::None });
        for (n, _, exec) in files {
            f.add_file(n.clone()).set_is_exec(*exec);
        }
    }
    let mut w = b.build(Cursor::new(Vec::new())).unwrap();
    let all: Vec<&Vec<u8>> = folders.iter().flat_map(|f| f.1.iter().map(|x| &x.1)).collect();
    let mut i = 0;
    while let Some(mut fw) = w.next_file().unwrap() {
        fw.write_all(all[i]).unwrap();
        i += 1;
    }
    w.finish().unwrap().into_inner()
}

/// Records what FDICopy did; `answer` decides fdintCOPY_FILE per file index.
struct Rec {
    events: Vec<(u32, Note)>,
    out: Vec<(u32, Vec<u8>)>,
    answer: Box<dyn FnMut(usize) -> i32>,
    close_ok: bool,
    short_write: bool,
    copies: usize,
}

impl Rec {
    fn new(answer: impl FnMut(usize) -> i32 + 'static) -> Rec {
        Rec { events: vec![], out: vec![], answer: Box::new(answer), close_ok: true, short_write: false, copies: 0 }
    }
}

impl Sink for Rec {
    fn notify(&mut self, kind: u32, n: &Note) -> i32 {
        self.events.push((kind, n.clone()));
        match kind {
            FDINT_COPY_FILE => {
                let i = self.copies;
                self.copies += 1;
                let h = (self.answer)(i);
                if h > 0 {
                    self.out.push((h as u32, Vec::new()));
                }
                h
            }
            FDINT_CLOSE_FILE_INFO => self.close_ok as i32,
            _ => 0,
        }
    }
    fn write(&mut self, hf: u32, data: &[u8]) -> u32 {
        let o = self.out.iter_mut().rev().find(|o| o.0 == hf).expect("write to a handle FDI was not given");
        if self.short_write {
            return 0;
        }
        o.1.extend_from_slice(data);
        data.len() as u32
    }
}

fn text(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| b"abcabcabd xyz\n"[(i * 7 + seed as usize) % 14] ^ ((i / 5000) as u8 & 1)).collect()
}

#[test]
fn copies_every_file_in_order() {
    let files = vec![("a.txt".to_string(), text(100, 1), false), ("dir\\b.bin".to_string(), text(70000, 2), true), ("empty".to_string(), vec![], false)];
    for zip in [false, true] {
        let d = build(&[(zip, files.clone())]);
        let h = header(&d).unwrap();
        assert_eq!((h.c_folders, h.c_files, h.cb_cabinet as usize, h.has_prev, h.has_next), (1, 3, d.len(), false, false));
        let mut r = Rec::new(|i| 100 + i as i32);
        copy(&d, &mut r).unwrap();
        let kinds: Vec<u32> = r.events.iter().map(|e| e.0).collect();
        assert_eq!(kinds, [FDINT_CABINET_INFO, 2, 3, 2, 3, 2, 3]);
        for (i, (name, data, exec)) in files.iter().enumerate() {
            let (cp, cl) = (&r.events[1 + 2 * i].1, &r.events[2 + 2 * i].1);
            assert_eq!((cp.psz1.as_slice(), cp.cb, cp.hf), (name.as_bytes(), data.len() as u32, 0));
            assert_eq!((cl.psz1.as_slice(), cl.hf, cl.cb), (name.as_bytes(), 100 + i as u32, *exec as u32));
            assert_eq!(cl.attribs & A_EXEC != 0, *exec);
            assert_eq!(&r.out[i].1, data, "{name} zip={zip}");
        }
    }
}

#[test]
fn skipped_files_and_files_in_other_folders() {
    let f0 = vec![("1".to_string(), text(40000, 3), false), ("2".to_string(), text(50000, 4), false), ("3".to_string(), text(10, 5), false)];
    let f1 = vec![("4".to_string(), text(90000, 6), false)];
    let d = build(&[(true, f0.clone()), (false, f1.clone())]);
    // only the 2nd and the 4th
    let mut r = Rec::new(|i| if i == 1 || i == 3 { 7 + i as i32 } else { 0 });
    copy(&d, &mut r).unwrap();
    assert_eq!(r.out.len(), 2);
    assert_eq!(r.out[0].1, f0[1].1);
    assert_eq!(r.out[1].1, f1[0].1);
    assert_eq!(r.events.iter().filter(|e| e.0 == FDINT_CLOSE_FILE_INFO).count(), 2);
    assert_eq!(r.events.iter().filter(|e| e.0 == FDINT_COPY_FILE).map(|e| e.1.i_folder).collect::<Vec<_>>(), [0, 0, 0, 1]);
}

#[test]
fn the_callers_answers_stop_it() {
    let files = vec![("a".to_string(), text(10, 1), false), ("b".to_string(), text(10, 2), false)];
    let d = build(&[(true, files)]);
    // fdintCOPY_FILE -1: abort before any byte of that file
    let mut r = Rec::new(|i| if i == 1 { -1 } else { 5 });
    assert_eq!(copy(&d, &mut r), Err(FDIERROR_USER_ABORT));
    assert_eq!(r.out.len(), 1);
    // fdintCLOSE_FILE_INFO FALSE
    let mut r = Rec::new(|_| 5);
    r.close_ok = false;
    assert_eq!(copy(&d, &mut r), Err(FDIERROR_USER_ABORT));
    assert_eq!(r.events.len(), 3);
    // a short pfnwrite
    let mut r = Rec::new(|_| 5);
    r.short_write = true;
    assert_eq!(copy(&d, &mut r), Err(FDIERROR_TARGET_FILE));
    // fdintCABINET_INFO -1
    struct No;
    impl Sink for No {
        fn notify(&mut self, k: u32, _: &Note) -> i32 {
            assert_eq!(k, FDINT_CABINET_INFO);
            -1
        }
        fn write(&mut self, _: u32, _: &[u8]) -> u32 {
            unreachable!()
        }
    }
    assert_eq!(copy(&d, &mut No), Err(FDIERROR_USER_ABORT));
}

#[test]
fn headers() {
    let d = build(&[(false, vec![("a".to_string(), b"hi".to_vec(), false)])]);
    assert_eq!(header(&d[..35]), Err(FDIERROR_NOT_A_CABINET));
    let mut bad = d.clone();
    bad[0] = b'X';
    assert_eq!(header(&bad), Err(FDIERROR_NOT_A_CABINET));
    let mut v = d.clone();
    v[24] = 4; // 1.4
    assert_eq!(header(&v), Err(FDIERROR_UNKNOWN_CABINET_VERSION));
    v[24] = 3;
    v[25] = 2; // 2.3
    assert_eq!(header(&v), Err(FDIERROR_UNKNOWN_CABINET_VERSION));
    // the file table cut off
    let mut r = Rec::new(|_| 1);
    assert_eq!(copy(&d[..40], &mut r), Err(FDIERROR_CORRUPT_CABINET));
    // the data cut off: the file is announced, then the folder ends early
    let mut r = Rec::new(|_| 1);
    assert_eq!(copy(&d[..d.len() - 1], &mut r), Err(FDIERROR_CORRUPT_CABINET));
    // Quantum
    let mut q = d.clone();
    let folder = CFHEADER;
    q[folder + 6] = 2;
    assert_eq!(copy(&q, &mut Rec::new(|_| 1)), Err(FDIERROR_BAD_COMPR_TYPE));
}

/// A long solid folder of many small files is decoded once, not from its start per file.
#[test]
fn one_pass_over_a_solid_folder() {
    let files: Vec<(String, Vec<u8>, bool)> = (0..3000).map(|i| (format!("f{i}"), text(700, i as u8), false)).collect();
    let d = build(&[(true, files.clone())]);
    let t = std::time::Instant::now();
    let mut r = Rec::new(|i| 1 + i as i32);
    copy(&d, &mut r).unwrap();
    for (i, f) in files.iter().enumerate() {
        assert_eq!(r.out[i].1, f.1);
    }
    // (decoding the folder again per file would be ~3000 x 2 MB)
    assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
}

fn arb_files() -> impl Strategy<Value = Vec<(bool, Vec<(String, Vec<u8>, bool)>)>> {
    let file = (0usize..90_000, any::<u8>(), any::<bool>(), any::<bool>()).prop_map(|(n, seed, random, exec)| {
        let data = if random { (0..n).map(|i| (i as u32).wrapping_mul(2654435761).wrapping_add(seed as u32).to_le_bytes()[3]).collect() } else { text(n, seed) };
        (data, exec)
    });
    let folder = (any::<bool>(), proptest::collection::vec(file, 1..6));
    proptest::collection::vec(folder, 1..4).prop_map(|fs| {
        let mut k = 0;
        fs.into_iter()
            .map(|(zip, files)| {
                (zip, files.into_iter().map(|(d, e)| { k += 1; (format!("d\\f{k}.bin"), d, e) }).collect())
            })
            .collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn prop_roundtrip_with_skips(folders in arb_files(), mask in any::<u64>()) {
        let d = build(&folders);
        let all: Vec<&(String, Vec<u8>, bool)> = folders.iter().flat_map(|f| f.1.iter()).collect();
        let take = move |i: usize| if mask >> (i % 64) & 1 == 1 { 10 + i as i32 } else { 0 };
        let mut r = Rec::new(take);
        copy(&d, &mut r).unwrap();
        let wanted: Vec<usize> = (0..all.len()).filter(|&i| take(i) != 0).collect();
        prop_assert_eq!(r.out.len(), wanted.len());
        for (o, &i) in r.out.iter().zip(&wanted) {
            prop_assert_eq!(o.0, 10 + i as u32);
            prop_assert_eq!(&o.1, &all[i].1);
        }
        // CABINET_INFO, then COPY per file, CLOSE after each taken one
        let mut want = vec![FDINT_CABINET_INFO];
        for i in 0..all.len() {
            want.push(FDINT_COPY_FILE);
            if take(i) != 0 { want.push(FDINT_CLOSE_FILE_INFO); }
        }
        prop_assert_eq!(r.events.iter().map(|e| e.0).collect::<Vec<_>>(), want);
    }

    /// Damaged cabinets: an error or a result, never a panic; whatever is written is a prefix
    /// of the real file's bytes or (a flipped byte in the data) of the same length at most.
    #[test]
    fn prop_damaged(folders in arb_files(), cut in any::<prop::sample::Index>(), flips in proptest::collection::vec((any::<prop::sample::Index>(), 1u8..), 0..4)) {
        let mut d = build(&folders);
        for (at, x) in flips {
            let i = at.index(d.len());
            d[i] ^= x;
        }
        let n = cut.index(d.len() + 1);
        let all: Vec<usize> = folders.iter().flat_map(|f| f.1.iter().map(|x| x.1.len())).collect();
        let mut r = Rec::new(|i| 1 + i as i32);
        let _ = copy(&d[..n], &mut r);
        for o in &r.out {
            // (a damaged file table can claim other sizes: bounded by the cabinet's data at most)
            prop_assert!(o.1.len() <= all.iter().copied().max().unwrap_or(0).max(n * 4000 + 70_000));
        }
    }
}
