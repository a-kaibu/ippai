//! Shared namespace snapshots. Handles and their offsets remain process-local.
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
impl Fs {
    fn shared_snapshot(&self) -> Vec<u8> {
        let mut out = b"VFS1".to_vec();
        u32put(&mut out, self.nodes.len() as u32);
        for node in &self.nodes {
            let Some(n) = node else { u32put(&mut out, 0); continue };
            let tag = match &n.data { Data::Dir(_) => 1, Data::Link(_) => 2, Data::File(Content::Upper(_)) => 3, Data::File(Content::Lower { .. }) => 4 };
            u32put(&mut out, tag); u32put(&mut out, n.mode); out.extend(n.mtime.to_le_bytes()); u32put(&mut out, n.nlink); u32put(&mut out, n.open_refs);
            match &n.data {
                Data::Dir(children) => { u32put(&mut out, children.len() as u32); for (name, ino) in children { bytes(&mut out, name.as_bytes()); u32put(&mut out, *ino); } }
                Data::Link(path) => bytes(&mut out, path.as_bytes()),
                Data::File(Content::Upper(data)) => bytes(&mut out, data),
                Data::File(Content::Lower { size, chunks, starts }) => {
                    out.extend(size.to_le_bytes()); u32put(&mut out, chunks.len() as u32);
                    for (i, hash) in chunks.iter().enumerate() { out.push(hash.is_some() as u8); if let Some(h) = hash { out.extend(h); } out.extend(starts[i + 1].to_le_bytes()); }
                }
            }
        }
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
            let tag = r.u32()?;
            if tag == 0 { nodes.push(None); free.push(i as u32); continue; }
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
            nodes.push(Some(Node { data, mode, mtime, nlink, open_refs }));
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
}

#[cfg(target_arch = "wasm32")]
unsafe extern "C" {
    fn vnhost_vfs_shared_lock() -> i32;
    fn vnhost_vfs_shared_unlock();
    fn vnhost_vfs_shared_generation() -> u32;
    fn vnhost_vfs_shared_get(dst: *mut u8, cap: usize) -> usize;
    fn vnhost_vfs_shared_put(src: *const u8, len: usize) -> i32;
}
pub(super) fn generation() -> u32 {
    #[cfg(target_arch = "wasm32")]
    { unsafe { vnhost_vfs_shared_generation() } }
    #[cfg(not(target_arch = "wasm32"))]
    { 0 }
}
pub(super) struct Guard { enabled: bool, before: u32, empty: bool }
impl Guard {
    pub(super) fn enter(fs: &mut Fs) -> Self {
        #[cfg(target_arch = "wasm32")]
        unsafe {
            if vnhost_vfs_shared_lock() == 0 { return Self { enabled: false, before: 0, empty: false }; }
            let guard = Self { enabled: true, before: CHANGES.load(std::sync::atomic::Ordering::Relaxed), empty: vnhost_vfs_shared_get(std::ptr::null_mut(), 0) == 0 };
            let generation = vnhost_vfs_shared_generation();
            if !guard.empty && fs.shared_generation != generation {
                let size = vnhost_vfs_shared_get(std::ptr::null_mut(), 0); let mut data = vec![0; size];
                assert_eq!(vnhost_vfs_shared_get(data.as_mut_ptr(), size), size);
                fs.apply_shared_snapshot(&data).expect("invalid shared filesystem snapshot");
                fs.shared_generation = generation;
            }
            return guard;
        }
        #[cfg(not(target_arch = "wasm32"))]
        { let _ = fs; Self { enabled: false, before: 0, empty: false } }
    }
    pub(super) fn publish(&self, fs: &mut Fs) {
        #[cfg(target_arch = "wasm32")]
        if self.enabled && (self.empty || self.before != CHANGES.load(std::sync::atomic::Ordering::Relaxed)) {
            let bytes = fs.shared_snapshot();
            unsafe { assert_ne!(vnhost_vfs_shared_put(bytes.as_ptr(), bytes.len()), 0, "shared filesystem capacity exceeded"); fs.shared_generation = vnhost_vfs_shared_generation(); }
        }
        #[cfg(not(target_arch = "wasm32"))]
        { let _ = (self.enabled, self.before, self.empty, fs); }
    }
}
impl Drop for Guard { fn drop(&mut self) { #[cfg(target_arch = "wasm32")] if self.enabled { unsafe { vnhost_vfs_shared_unlock(); } } } }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshots_preserve_remote_open_inodes_and_local_offsets() {
        let mut parent = Fs::default(); let root = parent.add(Data::Dir(BTreeMap::new()), 0o755, 0); parent.mounts.push(("/".into(), root));
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
    fn truncated_snapshots_do_not_mutate_the_receiver() {
        let mut source = Fs::default(); let root = source.add(Data::Dir(BTreeMap::new()), 0o755, 0); source.mounts.push(("/".into(), root)); source.put_file("/data", vec![1,2,3], 4);
        let bytes = source.shared_snapshot(); let mut receiver = Fs::default();
        for n in 0..bytes.len() { assert!(receiver.apply_shared_snapshot(&bytes[..n]).is_none()); assert!(receiver.nodes.is_empty()); }
        receiver.apply_shared_snapshot(&bytes).unwrap(); assert_eq!(receiver.shared_snapshot(), bytes);
    }
}
