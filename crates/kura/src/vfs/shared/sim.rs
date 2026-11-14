//! Processes sharing one namespace through a buffer in memory: what the property tests and the
//! fuzz target (fuzz/fuzz_targets/vfs_journal.rs) run. After every publish each process, having
//! taken in what is new, must have exactly the namespace of the one that published (the same as
//! a whole snapshot of it), its handles valid, its free slots the empty ones.
use super::*;

/// The host's buffer as a Vec (an upper bound like the host's maximum), counting the bytes that
/// went in and out.
#[derive(Default)]
#[allow(dead_code)]
pub(super) struct Mem {
    buf: Vec<u8>,
    len: usize,
    pub(super) generation: u32,
    max: usize,
    pub(super) written: usize,
    pub(super) read: std::cell::Cell<usize>,
}
impl Mem {
    pub(super) fn new(max: usize) -> Self { Mem { max, ..Default::default() } }
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

pub(super) fn rooted() -> (Fs, Ino) {
    let mut fs = Fs::default();
    let root = fs.add(Data::Dir(BTreeMap::new()), 0o755, 0);
    fs.mounts.push(("/".into(), root));
    (fs, root)
}

/// What one of the processes does.
#[derive(Debug, Clone)]
pub(super) enum Op {
    Put(u8, Vec<u8>),
    Write(u8, u16, Vec<u8>),
    Size(u8, u16),
    Open(u8),
    Close(u8),
    Unlink(u8),
    Mkdir(u8),
}
pub(super) fn name(n: u8) -> String { format!("/d{}/f{}", n % 3, n) }

pub(super) struct Sim {
    pub(super) m: Mem,
    ps: Vec<Fs>,
    root: Ino,
    lower: Ino,
    handles: Vec<Vec<u32>>,
}
impl Sim {
    pub(super) fn new(limit: usize) -> Self {
        let mut m = Mem::new(limit);
        let (mut first, root) = rooted();
        // (a file from the store too: its chunk list goes along)
        let lower = first.add(Data::File(Content::Lower { size: 10, chunks: Arc::new(vec![Some([3; 16]), None]), starts: Arc::new(vec![0, 4, 10]) }), 0o644, 0);
        if let Some(Data::Dir(ch)) = first.data_mut(root) { ch.insert("lower".into(), lower); }
        first.dirt.on = true;
        first.sync_in(&m);
        assert!(first.sync_out(&mut m));
        Sim { m, ps: vec![first, Fs::default(), Fs::default()], root, lower, handles: vec![vec![]; 3] }
    }

    /// Process `p` does `batch` under one publish; Err(what differs) if another process then
    /// does not have the same namespace; Ok(false) when the publish did not fit the buffer (the
    /// processes go separate ways from there: nothing more to compare).
    pub(super) fn step(&mut self, p: usize, batch: &[Op]) -> Result<bool, String> {
        let (root, lower) = (self.root, self.lower);
        let fs = &mut self.ps[p];
        let handles = &mut self.handles[p];
        fs.dirt.on = true;
        fs.sync_in(&self.m);
        for o in batch {
            match o {
                Op::Put(n, d) => fs.put_file(&name(*n), d.clone(), *n as i64),
                Op::Write(n, at, d) => if let Some(ino) = fs.walk(root, &name(*n)[1..]) { let _ = fs.write_upper(ino, d, *at as u64); },
                Op::Size(n, s) => if let Some(ino) = fs.walk(root, &name(*n)[1..]) { let _ = fs.set_upper_size(ino, *s as u64); },
                Op::Open(n) => if let Some(ino) = fs.walk(root, &name(*n)[1..]).or(if *n == 11 { Some(lower) } else { None }) { handles.push(fs.new_handle(ino, 0)); },
                Op::Close(i) => if !handles.is_empty() { let k = *i as usize % handles.len(); let h = handles.remove(k); fs.close_handle(h); },
                Op::Unlink(n) => { let path = name(*n); let (dir, base) = path[1..].split_once('/').unwrap(); if let Some(d) = fs.walk(root, dir) { fs.unlink_name(d, base); } }
                Op::Mkdir(n) => { fs.mkdir_p(root, &format!("d{}/sub{n}", n % 3)); }
            }
        }
        if !fs.sync_out(&mut self.m) {
            return Ok(false);
        }
        let want = self.ps[p].shared_snapshot();
        for q in 0..self.ps.len() {
            let fs = &mut self.ps[q];
            fs.sync_in(&self.m);
            if fs.shared_snapshot() != want { return Err(format!("process {q} after {batch:?} by {p}")); }
            for h in &self.handles[q] { if fs.node(fs.handles[h].ino).is_none() { return Err(format!("process {q}: handle {h} lost its node")); } }
            let mut free = fs.free.clone(); free.sort();
            let empty: Vec<Ino> = (0..fs.nodes.len() as Ino).filter(|&i| fs.nodes[i as usize].is_none()).collect();
            if free != empty { return Err(format!("process {q}: free slots {free:?}, empty {empty:?}")); }
        }
        Ok(true)
    }
}

/// The fuzz target: any bytes as entries (refused or applied whole), then as the processes'
/// operations (see `Sim`). Panics on what must not happen.
pub fn fuzz(data: &[u8]) {
    let (mut fs, _) = rooted();
    fs.put_file("/a/b", vec![1; 20], 0);
    let before = fs.shared_snapshot();
    if fs.apply_items(data).is_none() { assert!(fs.shared_snapshot() == before, "a refused entry changed the namespace"); }

    let Some((&first, mut rest)) = data.split_first() else { return };
    let limit = if first & 1 == 0 { 1 << 24 } else { 2000 + (first as usize) * 64 };
    let mut sim = Sim::new(limit);
    let mut take = |n: usize| -> Option<&[u8]> { if rest.len() < n { return None; } let (a, b) = rest.split_at(n); rest = b; Some(a) };
    while let Some(h) = take(2) {
        let (p, count) = ((h[0] % 3) as usize, 1 + (h[0] / 3 % 3) as usize);
        let mut batch = Vec::new();
        let mut k = h[1];
        for _ in 0..count {
            let Some(b) = take(3) else { break };
            let n = b[0] % 12;
            batch.push(match k % 7 {
                0 => { let len = (b[1] % 64) as usize; let d = take(len).map(<[u8]>::to_vec).unwrap_or_default(); Op::Put(n, d) }
                1 => { let len = (b[2] % 64) as usize; let d = take(len).map(<[u8]>::to_vec).unwrap_or_default(); Op::Write(n, b[1] as u16 * 3, d) }
                2 => Op::Size(n, b[1] as u16 * 4 + b[2] as u16 % 4),
                3 => Op::Open(n),
                4 => Op::Close(b[1]),
                5 => Op::Unlink(n),
                _ => Op::Mkdir(b[1] % 4),
            });
            k = b[2];
        }
        match sim.step(p, &batch) {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => panic!("{e}"),
        }
    }
}
