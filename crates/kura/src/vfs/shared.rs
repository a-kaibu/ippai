//! The namespace the processes share (once a process starts another: CreateProcess). Handles and
//! their offsets remain process-local.
//!
//! The shared buffer (host/vnfs.js, `vnvmFs`) holds a snapshot of every node and, after it, a
//! journal: what each process changed, as the changed nodes' new states (`Item`), appended under
//! the buffer's lock when that process's operation ends. A process reads only what was appended
//! since it last looked; it writes only the nodes it changed (`Dirt`): a write to a file in memory
//! sends the bytes written, an open or close the node's count of open handles. Once the journal
//! has grown past the snapshot's size (at least JOURNAL_MIN) the next change writes a new
//! snapshot instead, so a file written in pieces costs what was written, not its size each time
//! (review P1-3: 64 KiB writes to a 64 MiB file took about 0.1 s each, the whole namespace with
//! the file's bytes written and read back).
//!
//! Buffer layout: "VFS2", the snapshot's number (a new one makes the readers read it whole), its
//! length, the snapshot (`Fs::shared_snapshot`), then entries: length, items.
#![cfg_attr(not(any(target_arch = "wasm32", test)), allow(dead_code))]
use super::*;

fn u32put(out: &mut Vec<u8>, n: u32) { out.extend(n.to_le_bytes()); }
fn bytes(out: &mut Vec<u8>, b: &[u8]) { u32put(out, b.len() as u32); out.extend(b); }
struct Read<'a>(&'a [u8]);
impl<'a> Read<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> { let b = self.0.get(..n)?; self.0 = &self.0[n..]; Some(b) }
    fn u32(&mut self) -> Option<u32> { Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?)) }
    fn u64(&mut self) -> Option<u64> { Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?)) }
    fn bytes(&mut self) -> Option<Vec<u8>> { let n = self.u32()? as usize; Some(self.take(n)?.to_vec()) }
    fn string(&mut self) -> Option<String> { String::from_utf8(self.bytes()?).ok() }
}

fn put_node(out: &mut Vec<u8>, node: Option<&Node>) {
    let Some(n) = node else { u32put(out, 0); return };
    let tag = match &n.data { Data::Dir(_) => 1, Data::Link(_) => 2, Data::File(Content::Upper(_)) => 3, Data::File(Content::Lower { .. }) => 4 };
    u32put(out, tag); u32put(out, n.mode); out.extend(n.mtime.to_le_bytes()); u32put(out, n.nlink); u32put(out, n.open_refs);
    match &n.data {
        Data::Dir(children) => { u32put(out, children.len() as u32); for (name, ino) in children { bytes(out, name.as_bytes()); u32put(out, *ino); } }
        Data::Link(path) => bytes(out, path.as_bytes()),
        Data::File(Content::Upper(data)) => bytes(out, data),
        Data::File(Content::Lower { size, chunks, starts }) => {
            out.extend(size.to_le_bytes()); u32put(out, chunks.len() as u32);
            for (i, hash) in chunks.iter().enumerate() { out.push(hash.is_some() as u8); if let Some(h) = hash { out.extend(h); } out.extend(starts[i + 1].to_le_bytes()); }
        }
    }
}
/// A node as `put_node` wrote it: Some(None) for a free slot, None if the bytes are not one.
fn get_node(r: &mut Read) -> Option<Option<Node>> {
    let tag = r.u32()?;
    if tag == 0 { return Some(None); }
    let mode = r.u32()?; let mtime = r.u64()? as i64; let nlink = r.u32()?; let open_refs = r.u32()?;
    let data = match tag {
        1 => { let n = r.u32()?; if n as usize > r.0.len() / 8 { return None; } let mut children = BTreeMap::new(); for _ in 0..n { children.insert(r.string()?, r.u32()?); } Data::Dir(children) }
        2 => Data::Link(r.string()?),
        3 => Data::File(Content::Upper(r.bytes()?)),
        4 => {
            let size = r.u64()?; let n = r.u32()? as usize;
            if n > r.0.len() / 9 { return None; }
            let mut chunks = Vec::with_capacity(n); let mut starts = vec![0];
            for _ in 0..n { let hash = match r.take(1)?[0] { 0 => None, 1 => Some(r.take(16)?.try_into().ok()?), _ => return None }; chunks.push(hash); let end = r.u64()?; if end <= *starts.last()? { return None; } starts.push(end); }
            if *starts.last()? != size { return None; }
            Data::File(Content::Lower { size, chunks: Arc::new(chunks), starts: Arc::new(starts) })
        }
        _ => return None,
    };
    Some(Some(Node { data, mode, mtime, nlink, open_refs }))
}

/// How much of a node changed since it was last published.
#[derive(Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Level {
    /// its count of open handles
    #[default]
    Refs,
    /// its counts, times and mode (and a file in memory's size; its bytes are `Mark::bytes`)
    Meta,
    /// everything
    Whole,
}
#[derive(Default, Debug)]
struct Mark {
    level: Level,
    /// a file in memory's byte ranges written (merged; at most MAX_RANGES)
    bytes: Vec<(usize, usize)>,
}
const MAX_RANGES: usize = 16;

/// The nodes this process changed since it last published (only while the namespace is shared).
#[derive(Default, Debug)]
pub(super) struct Dirt {
    on: bool,
    nodes: BTreeMap<Ino, Mark>,
}
impl Dirt {
    fn mark(&mut self, ino: Ino, level: Level) -> Option<&mut Mark> {
        if !self.on { return None; }
        let m = self.nodes.entry(ino).or_default();
        m.level = m.level.max(level);
        Some(m)
    }
    pub(super) fn whole(&mut self, ino: Ino) { self.mark(ino, Level::Whole); }
    pub(super) fn meta(&mut self, ino: Ino) { self.mark(ino, Level::Meta); }
    pub(super) fn refs(&mut self, ino: Ino) { self.mark(ino, Level::Refs); }
    /// Bytes `lo..hi` of file in memory `ino` changed (its size too, maybe).
    pub(super) fn bytes(&mut self, ino: Ino, lo: usize, hi: usize) {
        let Some(m) = self.mark(ino, Level::Meta) else { return };
        if lo >= hi { return; }
        let r = &mut m.bytes;
        let i = r.partition_point(|x| x.1 < lo);
        let mut j = i;
        let (mut a, mut b) = (lo, hi);
        while j < r.len() && r[j].0 <= hi { a = a.min(r[j].0); b = b.max(r[j].1); j += 1; }
        r.splice(i..j, [(a, b)]);
        if r.len() > MAX_RANGES {
            // (scattered writes: the smallest gap goes)
            let k = (0..r.len() - 1).min_by_key(|&k| r[k + 1].0 - r[k].1).unwrap();
            r[k].1 = r[k + 1].1;
            r.remove(k + 1);
        }
    }
    pub(super) fn is_empty(&self) -> bool { self.nodes.is_empty() }
}

/// What a process has of the shared buffer: its generation, the snapshot's number, and where in
/// the journal it has read to (0: nothing yet).
#[derive(Default, Debug)]
pub(super) struct Synced {
    generation: u32,
    base: u32,
    off: usize,
    /// the snapshot's length (where the journal starts)
    snap: usize,
    mounts: usize,
}

/// A node's new state in an entry.
enum Item {
    /// all of it (or the slot free)
    Node(Ino, Option<Node>),
    /// a file in memory's counts, size, and the bytes that changed
    Upper { ino: Ino, mode: u32, mtime: i64, nlink: u32, open_refs: u32, size: u64, ranges: Vec<(u64, Vec<u8>)> },
    /// the count of open handles
    Refs(Ino, u32),
}
const I_NODE: u8 = 1;
const I_UPPER: u8 = 2;
const I_REFS: u8 = 3;

/// The journal grows to at least this before a new snapshot replaces it.
#[cfg(not(test))]
const JOURNAL_MIN: usize = 1 << 20;
#[cfg(test)]
const JOURNAL_MIN: usize = 300;
const HEAD: usize = 12;

/// The shared buffer.
pub(super) trait Region {
    /// bytes in use (0: nobody published yet)
    fn len(&self) -> usize;
    fn generation(&self) -> u32;
    fn read(&self, off: usize, dst: &mut [u8]);
    /// false if the buffer cannot hold `off + src.len()` bytes
    fn write(&mut self, off: usize, src: &[u8]) -> bool;
    /// `len` bytes are now in use: a new generation
    fn commit(&mut self, len: usize) -> u32;
}

impl Fs {
    fn shared_snapshot(&self) -> Vec<u8> {
        let mut out = b"VFS1".to_vec();
        u32put(&mut out, self.nodes.len() as u32);
        for node in &self.nodes { put_node(&mut out, node.as_ref()); }
        u32put(&mut out, self.mounts.len() as u32);
        for (path, ino) in &self.mounts { bytes(&mut out, path.as_bytes()); u32put(&mut out, *ino); }
        out
    }
    fn apply_shared_snapshot(&mut self, data: &[u8]) -> Option<()> {
        let mut r = Read(data);
        if r.take(4)? != b"VFS1" { return None; }
        let count = r.u32()? as usize;
        if count > r.0.len() / 4 { return None; }
        let mut nodes = Vec::with_capacity(count);
        let mut free = Vec::new();
        for i in 0..count {
            let n = get_node(&mut r)?;
            if n.is_none() { free.push(i as u32); }
            nodes.push(n);
        }
        let n = r.u32()?; if n as usize > r.0.len() / 8 { return None; }
        let mut mounts = Vec::new(); for _ in 0..n { mounts.push((r.string()?, r.u32()?)); }
        if !r.0.is_empty() { return None; }
        let valid = |ino: Ino| nodes.get(ino as usize).is_some_and(Option::is_some);
        if mounts.iter().any(|(_, ino)| !valid(*ino)) || self.handles.values().any(|h| !valid(h.ino)) { return None; }
        for node in nodes.iter().flatten() { if let Data::Dir(ch) = &node.data { if ch.values().any(|ino| !valid(*ino)) { return None; } } }
        self.nodes = nodes; self.mounts = mounts; self.free = free;
        Some(())
    }

    /// The items for what this process changed (and forgets it).
    fn take_items(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut count = 0u32;
        u32put(&mut out, 0);
        for (ino, mark) in std::mem::take(&mut self.dirt.nodes) {
            let node = self.node(ino);
            if node.is_none() && ino as usize >= self.nodes.len() { continue; }
            count += 1;
            match (node, mark.level) {
                (Some(n), Level::Refs) => { out.push(I_REFS); u32put(&mut out, ino); u32put(&mut out, n.open_refs); }
                (Some(n @ Node { data: Data::File(Content::Upper(v)), .. }), Level::Meta) => {
                    out.push(I_UPPER); u32put(&mut out, ino);
                    u32put(&mut out, n.mode); out.extend(n.mtime.to_le_bytes()); u32put(&mut out, n.nlink); u32put(&mut out, n.open_refs);
                    out.extend((v.len() as u64).to_le_bytes());
                    let ranges: Vec<_> = mark.bytes.iter().map(|&(a, b)| (a.min(v.len()), b.min(v.len()))).filter(|r| r.0 < r.1).collect();
                    u32put(&mut out, ranges.len() as u32);
                    for (a, b) in ranges { out.extend((a as u64).to_le_bytes()); bytes(&mut out, &v[a..b]); }
                }
                (n, _) => { out.push(I_NODE); u32put(&mut out, ino); put_node(&mut out, n); }
            }
        }
        out[..4].copy_from_slice(&count.to_le_bytes());
        out
    }

    /// Applies one entry's items, all or none (None: they are not ones `take_items` wrote, or
    /// they leave a name or a handle here without its node).
    fn apply_items(&mut self, data: &[u8]) -> Option<()> {
        let mut r = Read(data);
        let n = r.u32()? as usize;
        if n > r.0.len() / 5 { return None; }
        let mut items = Vec::with_capacity(n);
        for _ in 0..n {
            let kind = r.take(1)?[0];
            let ino = r.u32()?;
            items.push(match kind {
                I_NODE => Item::Node(ino, get_node(&mut r)?),
                I_REFS => Item::Refs(ino, r.u32()?),
                I_UPPER => {
                    let (mode, mtime, nlink, open_refs, size) = (r.u32()?, r.u64()? as i64, r.u32()?, r.u32()?, r.u64()?);
                    let k = r.u32()? as usize;
                    if k > r.0.len() / 12 { return None; }
                    let mut ranges = Vec::with_capacity(k);
                    for _ in 0..k {
                        let at = r.u64()?; let b = r.bytes()?;
                        if at.checked_add(b.len() as u64)? > size { return None; }
                        ranges.push((at, b));
                    }
                    Item::Upper { ino, mode, mtime, nlink, open_refs, size, ranges }
                }
                _ => return None,
            });
        }
        if !r.0.is_empty() { return None; }
        // (checked against the state the items make before anything changes)
        let mut after: HashMap<Ino, bool> = HashMap::new();
        for it in &items {
            match it {
                Item::Node(ino, n) => { after.insert(*ino, n.is_some()); }
                Item::Refs(ino, _) => if !matches!(self.node(*ino), Some(_)) && after.get(ino) != Some(&true) { return None; },
                Item::Upper { ino, size, .. } => {
                    let upper = matches!(self.node(*ino), Some(Node { data: Data::File(Content::Upper(_)), .. }));
                    if !upper || after.contains_key(ino) || usize::try_from(*size).is_err() { return None; }
                }
            }
        }
        let valid = |ino: Ino| after.get(&ino).copied().unwrap_or_else(|| self.node(ino).is_some());
        for it in &items {
            if let Item::Node(_, Some(Node { data: Data::Dir(ch), .. })) = it {
                if ch.values().any(|i| !valid(*i)) { return None; }
            }
        }
        if self.mounts.iter().any(|(_, i)| !valid(*i)) || self.handles.values().any(|h| !valid(h.ino)) { return None; }
        let mut slots = false;
        for it in items {
            match it {
                Item::Node(ino, n) => {
                    let i = ino as usize;
                    if i >= self.nodes.len() { self.nodes.resize_with(i + 1, || None); slots = true; }
                    slots |= self.nodes[i].is_some() != n.is_some();
                    self.nodes[i] = n;
                }
                Item::Refs(ino, refs) => if let Some(Some(n)) = self.nodes.get_mut(ino as usize) { n.open_refs = refs },
                Item::Upper { ino, mode, mtime, nlink, open_refs, size, ranges } => {
                    let Some(Some(n)) = self.nodes.get_mut(ino as usize) else { continue };
                    (n.mode, n.mtime, n.nlink, n.open_refs) = (mode, mtime, nlink, open_refs);
                    if let Data::File(Content::Upper(v)) = &mut n.data {
                        v.resize(size as usize, 0);
                        for (at, b) in ranges { v[at as usize..at as usize + b.len()].copy_from_slice(&b); }
                    }
                }
            }
        }
        if slots { self.free = (0..self.nodes.len() as Ino).filter(|&i| self.nodes[i as usize].is_none()).collect(); }
        Some(())
    }

    /// Takes in what the other processes changed since this one last looked.
    fn sync_in(&mut self, r: &dyn Region) {
        let len = r.len();
        let generation = r.generation();
        if len == 0 || (self.shared.generation == generation && self.shared.off != 0) { return; }
        let mut head = [0u8; HEAD];
        r.read(0, &mut head);
        assert_eq!(&head[..4], b"VFS2", "invalid shared filesystem");
        let base = u32::from_le_bytes(head[4..8].try_into().unwrap());
        let snap = u32::from_le_bytes(head[8..12].try_into().unwrap()) as usize;
        if self.shared.off == 0 || self.shared.base != base {
            let mut data = vec![0; snap];
            r.read(HEAD, &mut data);
            self.apply_shared_snapshot(&data).expect("invalid shared filesystem snapshot");
            self.shared = Synced { generation, base, off: HEAD + snap, snap, mounts: self.mounts.len() };
        }
        if self.shared.off < len {
            let mut data = vec![0; len - self.shared.off];
            r.read(self.shared.off, &mut data);
            let mut at = 0;
            while at < data.len() {
                let n = u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize;
                self.apply_items(&data[at + 4..at + 4 + n]).expect("invalid shared filesystem entry");
                at += 4 + n;
            }
        }
        self.shared.off = len;
        self.shared.generation = generation;
    }

    /// Publishes what this process changed: an entry at the journal's end, or a new snapshot
    /// (the first time, past the journal's size, or when the entry does not fit). False when even
    /// the snapshot does not fit: the others keep the namespace as it was.
    fn sync_out(&mut self, r: &mut dyn Region) -> bool {
        if self.dirt.is_empty() && r.len() != 0 && self.shared.mounts == self.mounts.len() { return true; }
        if r.len() != 0 && self.shared.off == r.len() && self.shared.mounts == self.mounts.len() {
            let items = self.take_items();
            let journal = self.shared.off - HEAD - self.shared.snap;
            if journal + 4 + items.len() <= self.shared.snap.max(JOURNAL_MIN) {
                let mut entry = Vec::with_capacity(4 + items.len());
                u32put(&mut entry, items.len() as u32);
                entry.extend(items);
                if r.write(self.shared.off, &entry) {
                    let len = self.shared.off + entry.len();
                    self.shared.generation = r.commit(len);
                    self.shared.off = len;
                    return true;
                }
            }
        }
        self.dirt.nodes.clear();
        let snap = self.shared_snapshot();
        let base = if r.len() == 0 { 1 } else { self.shared.base.wrapping_add(1) };
        let mut out = Vec::with_capacity(HEAD + snap.len());
        out.extend(b"VFS2"); u32put(&mut out, base); u32put(&mut out, snap.len() as u32); out.extend(&snap);
        if !r.write(0, &out) { return false; }
        let generation = r.commit(out.len());
        self.shared = Synced { generation, base, off: out.len(), snap: snap.len(), mounts: self.mounts.len() };
        true
    }
}

#[cfg(target_arch = "wasm32")]
unsafe extern "C" {
    fn vnhost_vfs_shared_lock() -> i32;
    fn vnhost_vfs_shared_unlock();
    fn vnhost_vfs_shared_generation() -> u32;
    fn vnhost_vfs_shared_len() -> usize;
    fn vnhost_vfs_shared_read(dst: *mut u8, off: usize, len: usize);
    fn vnhost_vfs_shared_write(src: *const u8, off: usize, len: usize) -> i32;
    fn vnhost_vfs_shared_commit(len: usize) -> u32;
    fn vnhost_vfs_shared_enable() -> i32;
}
/// host/vnfs.js's buffer (taken by Guard::enter)
#[cfg(target_arch = "wasm32")]
struct Host;
#[cfg(target_arch = "wasm32")]
impl Region for Host {
    fn len(&self) -> usize { unsafe { vnhost_vfs_shared_len() } }
    fn generation(&self) -> u32 { unsafe { vnhost_vfs_shared_generation() } }
    fn read(&self, off: usize, dst: &mut [u8]) { unsafe { vnhost_vfs_shared_read(dst.as_mut_ptr(), off, dst.len()) } }
    fn write(&mut self, off: usize, src: &[u8]) -> bool { unsafe { vnhost_vfs_shared_write(src.as_ptr(), off, src.len()) != 0 } }
    fn commit(&mut self, len: usize) -> u32 { unsafe { vnhost_vfs_shared_commit(len) } }
}

/// Starts sharing the namespace with the child processes to come (the first CreateProcess);
/// false where there is no shared buffer (node, tests).
pub(super) fn enable() -> bool {
    #[cfg(target_arch = "wasm32")]
    unsafe { return vnhost_vfs_shared_enable() != 0; }
    #[cfg(not(target_arch = "wasm32"))]
    false
}
pub(super) fn generation() -> u32 {
    #[cfg(target_arch = "wasm32")]
    { unsafe { vnhost_vfs_shared_generation() } }
    #[cfg(not(target_arch = "wasm32"))]
    { 0 }
}
pub(super) struct Guard { enabled: bool }
impl Guard {
    pub(super) fn enter(fs: &mut Fs) -> Self {
        #[cfg(target_arch = "wasm32")]
        unsafe {
            if vnhost_vfs_shared_lock() == 0 { return Self { enabled: false }; }
            fs.dirt.on = true;
            fs.sync_in(&Host);
            return Self { enabled: true };
        }
        #[cfg(not(target_arch = "wasm32"))]
        { let _ = fs; Self { enabled: false } }
    }
    pub(super) fn publish(&self, fs: &mut Fs) {
        #[cfg(target_arch = "wasm32")]
        if self.enabled && !fs.sync_out(&mut Host) {
            // (past the buffer's maximum: this process goes on with its own files; the
            // others keep the last namespace that fitted rather than the game stopping)
            static TOLD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !TOLD.swap(true, std::sync::atomic::Ordering::Relaxed) {
                eprintln!("kura: the shared file system does not fit its buffer; the other processes keep the previous one");
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        { let _ = (self.enabled, fs); }
    }
}
impl Drop for Guard { fn drop(&mut self) { #[cfg(target_arch = "wasm32")] if self.enabled { unsafe { vnhost_vfs_shared_unlock(); } } } }

#[cfg(test)]
mod tests;
