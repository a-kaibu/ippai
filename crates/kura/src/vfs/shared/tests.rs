use super::*;
use proptest::prelude::*;

/// The host's buffer as a Vec (an upper bound like the host's maximum), counting the bytes that
/// went in and out.
#[derive(Default)]
struct Mem {
    buf: Vec<u8>,
    len: usize,
    generation: u32,
    max: usize,
    written: usize,
    read: std::cell::Cell<usize>,
}
impl Mem {
    fn new(max: usize) -> Self { Mem { max, ..Default::default() } }
}
impl Region for Mem {
    fn len(&self) -> usize { self.len }
    fn generation(&self) -> u32 { self.generation }
    fn read(&self, off: usize, dst: &mut [u8]) { self.read.set(self.read.get() + dst.len()); dst.copy_from_slice(&self.buf[off..off + dst.len()]); }
    fn write(&mut self, off: usize, src: &[u8]) -> bool {
        if off + src.len() > self.max { return false; }
        if self.buf.len() < off + src.len() { self.buf.resize(off + src.len(), 0); }
        self.buf[off..off + src.len()].copy_from_slice(src);
        self.written += src.len();
        true
    }
    fn commit(&mut self, len: usize) -> u32 { self.len = len; self.generation += 1; self.generation }
}

fn rooted() -> (Fs, Ino) {
    let mut fs = Fs::default();
    let root = fs.add(Data::Dir(BTreeMap::new()), 0o755, 0);
    fs.mounts.push(("/".into(), root));
    (fs, root)
}
/// One process's operation as vfs::with runs it: take in the others' changes, do it, publish.
fn op<R>(fs: &mut Fs, m: &mut Mem, f: impl FnOnce(&mut Fs) -> R) -> R {
    fs.dirt.on = true;
    fs.sync_in(m);
    let r = f(fs);
    assert!(fs.sync_out(m));
    r
}

#[test]
fn snapshots_preserve_remote_open_inodes_and_local_offsets() {
    let (mut parent, root) = rooted();
    parent.put_file("/file", b"first".to_vec(), 1);
    let ino = parent.walk(root, "file").unwrap(); let h = parent.new_handle(ino, 0); parent.handles.get_mut(&h).unwrap().pos = 3;
    let mut child = Fs::default(); child.apply_shared_snapshot(&parent.shared_snapshot()).unwrap(); child.unlink_name(root, "file");
    assert!(child.node(ino).is_some());
    child.put_file("/next", b"second".to_vec(), 2);
    assert_ne!(child.walk(root, "next"), Some(ino));
    parent.apply_shared_snapshot(&child.shared_snapshot()).unwrap(); assert_eq!(parent.handles[&h].pos, 3);
    let handle = parent.handles.remove(&h).unwrap(); parent.node_mut(handle.ino).unwrap().open_refs -= 1; parent.gc(handle.ino); assert!(parent.node(ino).is_none());
}

#[test]
fn opening_counts_the_node_and_sends_only_the_count() {
    let (mut fs, root) = rooted();
    fs.put_file("/save.dat", vec![5; 1000], 1);
    let ino = fs.walk(root, "save.dat").unwrap();
    let mut m = Mem::new(1 << 20);
    op(&mut fs, &mut m, |_| ());
    let before = m.written;
    let h = op(&mut fs, &mut m, |fs| fs.new_handle(ino, 0));
    assert_eq!(fs.node(ino).unwrap().open_refs, 1);
    // (length, count, kind, inode, open handles)
    assert_eq!(m.written - before, 4 + 4 + 1 + 4 + 4);
    let mut other = Fs::default();
    other.sync_in(&m);
    assert_eq!(other.node(ino).unwrap().open_refs, 1);
    op(&mut fs, &mut m, |fs| fs.close_handle(h));
    other.sync_in(&m);
    assert_eq!(other.node(ino).unwrap().open_refs, 0);
}

#[test]
fn truncated_snapshots_do_not_mutate_the_receiver() {
    let (mut source, _) = rooted(); source.put_file("/data", vec![1,2,3], 4);
    let bytes = source.shared_snapshot(); let mut receiver = Fs::default();
    for n in 0..bytes.len() { assert!(receiver.apply_shared_snapshot(&bytes[..n]).is_none()); assert!(receiver.nodes.is_empty()); }
    receiver.apply_shared_snapshot(&bytes).unwrap(); assert_eq!(receiver.shared_snapshot(), bytes);
}

#[test]
fn cut_or_broken_entries_change_nothing() {
    let (mut a, root) = rooted();
    let mut m = Mem::new(1 << 20);
    op(&mut a, &mut m, |fs| fs.put_file("/f", vec![1; 10], 1));
    let ino = a.walk(root, "f").unwrap();
    let mut b = Fs::default();
    b.sync_in(&m);
    a.dirt.on = true;
    a.write_upper(ino, b"xyz", 20).unwrap();
    a.mkdir_p(root, "d/e");
    let items = a.take_items();
    let before = b.shared_snapshot();
    for n in 0..items.len() {
        assert!(b.apply_items(&items[..n]).is_none(), "cut at {n}");
        assert_eq!(b.shared_snapshot(), before);
    }
    // (a directory naming a node that is not there)
    let mut bad = Vec::new();
    u32put(&mut bad, 1); bad.push(I_NODE); u32put(&mut bad, root);
    let mut dir = BTreeMap::new(); dir.insert("ghost".to_string(), 999);
    put_node(&mut bad, Some(&Node { data: Data::Dir(dir), mode: 0o755, mtime: 0, nlink: 2, open_refs: 0 }));
    assert!(b.apply_items(&bad).is_none());
    assert_eq!(b.shared_snapshot(), before);
    b.apply_items(&items).unwrap();
    assert_eq!(b.shared_snapshot(), a.shared_snapshot());
}

/// The case the review measured (P1-3): a 20k-file tree and a 64 MiB file written 64 KiB at a
/// time while another process shares the namespace. What goes through the buffer is about what
/// was written (once more for the snapshots that replace the journal), not the namespace per
/// write; an open and a close are a few bytes.
#[test]
fn writes_cost_what_they_write() {
    let (mut a, root) = rooted();
    for i in 0..20_000 { a.put_file(&format!("/data/{}/{i}.ks", i % 100), vec![], 0); }
    let mut m = Mem::new(1 << 30);
    op(&mut a, &mut m, |fs| fs.put_file("/save/big.bin", vec![], 1));
    let ino = a.walk(root, "save/big.bin").unwrap();
    let mut b = Fs::default();
    b.sync_in(&m);
    let start = (m.written, m.read.get());
    let piece = vec![0xa5u8; 64 << 10];
    for k in 0..1024u64 {
        op(&mut a, &mut m, |fs| fs.write_upper(ino, &piece, k * piece.len() as u64).unwrap());
        // (the other process opens and closes something now and then, reading what is new)
        if k % 64 == 0 {
            let f = b.walk(root, "data/7/7.ks").unwrap();
            let before = m.written;
            let h = op(&mut b, &mut m, |fs| fs.new_handle(f, 0));
            op(&mut b, &mut m, |fs| fs.close_handle(h));
            assert!(m.written - before < 100, "an open and a close wrote {}", m.written - before);
        }
    }
    b.sync_in(&m);
    assert_eq!(b.file_size(ino), 64 << 20);
    assert_eq!(b.shared_snapshot(), a.shared_snapshot());
    let (w, r) = (m.written - start.0, m.read.get() - start.1);
    // 64 MiB written: the journal, plus a snapshot each time it outgrows the last one
    assert!(w < 3 * (64 << 20) + (8 << 20), "{} MiB went into the buffer", w >> 20);
    assert!(r < 3 * (64 << 20) + (8 << 20), "{} MiB were read back", r >> 20);
}

#[test]
fn a_buffer_too_small_keeps_the_last_namespace() {
    let (mut a, root) = rooted();
    let mut m = Mem::new(400);
    op(&mut a, &mut m, |fs| fs.put_file("/x", vec![1; 10], 1));
    let gen = m.generation;
    a.dirt.on = true;
    a.sync_in(&m);
    let ino = a.walk(root, "x").unwrap();
    a.write_upper(ino, &[2; 1000], 0).unwrap();
    assert!(!a.sync_out(&mut m));
    assert_eq!(m.generation, gen);
    let mut b = Fs::default();
    b.sync_in(&m);
    assert_eq!(b.file_size(ino), 10);
    // (and the process that could not publish keeps its own files)
    a.sync_in(&m);
    assert_eq!(a.file_size(ino), 1000);
}

/// What one of the processes does.
#[derive(Debug, Clone)]
enum Op {
    Put(u8, Vec<u8>),
    Write(u8, u16, Vec<u8>),
    Size(u8, u16),
    Open(u8),
    Close(u8),
    Unlink(u8),
    Mkdir(u8),
}
fn ops() -> impl Strategy<Value = Vec<(u8, Vec<Op>)>> {
    let data = || prop::collection::vec(any::<u8>(), 0..200);
    let op = prop_oneof![
        (0u8..12, data()).prop_map(|(n, d)| Op::Put(n, d)),
        (0u8..12, 0u16..600, data()).prop_map(|(n, o, d)| Op::Write(n, o, d)),
        (0u8..12, 0u16..900).prop_map(|(n, s)| Op::Size(n, s)),
        (0u8..12).prop_map(Op::Open),
        (0u8..8).prop_map(Op::Close),
        (0u8..12).prop_map(Op::Unlink),
        (0u8..4).prop_map(Op::Mkdir),
    ];
    // (one to three of them by one process before it publishes: what one publish carries)
    prop::collection::vec((0u8..3, prop::collection::vec(op, 1..4)), 1..60)
}
fn name(n: u8) -> String { format!("/d{}/f{}", n % 3, n) }

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]
    /// Three processes doing anything to one namespace through the journal: after every
    /// operation each of them, having taken in what is new, has exactly the namespace the
    /// one that did it has (the same as a whole snapshot of it); handles stay each one's own.
    #[test]
    fn processes_agree(ops in ops(), limit in prop_oneof![Just(usize::MAX), 2000usize..20000]) {
        let mut m = Mem::new(limit.min(1 << 24));
        let (first, root) = rooted();
        let mut ps = vec![first, Fs::default(), Fs::default()];
        // (a file from the store too: its chunk list goes along)
        let lower = ps[0].add(Data::File(Content::Lower { size: 10, chunks: Arc::new(vec![Some([3; 16]), None]), starts: Arc::new(vec![0, 4, 10]) }), 0o644, 0);
        if let Some(Data::Dir(ch)) = ps[0].data_mut(root) { ch.insert("lower".into(), lower); }
        let mut handles: Vec<Vec<u32>> = vec![vec![]; 3];
        op(&mut ps[0], &mut m, |_| ());
        for (who, batch) in ops {
            let p = who as usize;
            let fs = &mut ps[p];
            fs.dirt.on = true;
            fs.sync_in(&m);
            for o in &batch {
                match o {
                    Op::Put(n, d) => fs.put_file(&name(*n), d.clone(), *n as i64),
                    Op::Write(n, at, d) => if let Some(ino) = fs.walk(root, &name(*n)[1..]) { let _ = fs.write_upper(ino, d, *at as u64); },
                    Op::Size(n, s) => if let Some(ino) = fs.walk(root, &name(*n)[1..]) { let _ = fs.set_upper_size(ino, *s as u64); },
                    Op::Open(n) => if let Some(ino) = fs.walk(root, &name(*n)[1..]).or(if *n == 11 { Some(lower) } else { None }) { handles[p].push(fs.new_handle(ino, 0)); },
                    Op::Close(i) => if !handles[p].is_empty() { let k = *i as usize % handles[p].len(); let h = handles[p].remove(k); fs.close_handle(h); },
                    Op::Unlink(n) => { let path = name(*n); let (dir, base) = path[1..].split_once('/').unwrap(); if let Some(d) = fs.walk(root, dir) { fs.unlink_name(d, base); } }
                    Op::Mkdir(n) => { fs.mkdir_p(root, &format!("d{}/sub{n}", n % 3)); }
                }
            }
            let o = &batch;
            if !fs.sync_out(&mut m) {
                // (past the buffer: the others keep what they had; this one goes on alone, so
                // the comparison ends here)
                return Ok(());
            }
            let want = ps[p].shared_snapshot();
            for q in 0..3 {
                ps[q].sync_in(&m);
                prop_assert_eq!(&ps[q].shared_snapshot(), &want, "process {} after {:?} by {}", q, o, p);
                for h in &handles[q] { prop_assert!(ps[q].node(ps[q].handles[h].ino).is_some()); }
                // the free slots are the empty ones
                let mut free = ps[q].free.clone(); free.sort();
                let empty: Vec<Ino> = (0..ps[q].nodes.len() as Ino).filter(|&i| ps[q].nodes[i as usize].is_none()).collect();
                prop_assert_eq!(free, empty);
            }
        }
    }

    /// Entries of any bytes are refused or applied whole, never half; they never panic.
    #[test]
    fn any_entry_bytes(b in prop::collection::vec(any::<u8>(), 0..300)) {
        let (mut fs, _) = rooted();
        fs.put_file("/a/b", vec![1; 20], 0);
        let before = fs.shared_snapshot();
        if fs.apply_items(&b).is_none() { prop_assert_eq!(fs.shared_snapshot(), before); }
    }
}

/// Byte ranges merge and stay few.
#[test]
fn dirt_ranges_merge() {
    let mut d = Dirt { on: true, ..Default::default() };
    d.bytes(1, 10, 20);
    d.bytes(1, 30, 40);
    d.bytes(1, 20, 30);
    assert_eq!(d.nodes[&1].bytes, [(10, 40)]);
    d.bytes(1, 0, 5);
    d.bytes(1, 50, 60);
    assert_eq!(d.nodes[&1].bytes, [(0, 5), (10, 40), (50, 60)]);
    for i in 0..100 { d.bytes(2, i * 10, i * 10 + 1); }
    assert!(d.nodes[&2].bytes.len() <= MAX_RANGES);
    let covered: usize = d.nodes[&2].bytes.iter().map(|r| r.1 - r.0).sum();
    assert!(covered >= 100);
    for i in 0..100 { assert!(d.nodes[&2].bytes.iter().any(|r| r.0 <= i * 10 && i * 10 + 1 <= r.1)); }
    let mut off = Dirt::default();
    off.bytes(1, 0, 5);
    assert!(off.is_empty());
}
