//! The page's file system: directory trees packed by `ippai pack` (`trees/<name>.tsv` plus
//! chunks in the store) mounted where `IPPAI_MOUNTS` says ("/=:/ippai/game=reunion"), read
//! lazily chunk by chunk. Whatever is written stays in memory. Paths outside the mounts are the
//! host's (node; the page mounts an empty tree at "/", so there is nothing outside). Rust code
//! reaches it through crate::fs.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::{Arc, Mutex};

use libc::size_t;

pub mod hints;
pub use foresee as predict;
pub mod trace;
#[cfg(test)]
mod replay;
mod shared;

type Ino = u32;
type Hash = [u8; 16];

enum Content {
    /// From the store; `None` chunks are all zero.
    Lower { size: u64, chunks: Arc<Vec<Option<Hash>>>, starts: Arc<Vec<u64>> },
    Upper(Vec<u8>),
}

/// Where a store file's chunks lie (cut by content: ippai's cdc.rs): where each starts, and the
/// file's size last.
#[derive(Clone)]
struct Layout {
    size: u64,
    starts: Arc<Vec<u64>>,
}

impl Layout {
    /// The chunk that holds byte `at`.
    fn index(&self, at: u64) -> usize {
        self.starts.partition_point(|&x| x <= at) - 1
    }

    /// Where chunk `i` starts, and its length.
    fn span(&self, i: usize) -> (u64, u64) {
        (self.starts[i], self.starts[i + 1] - self.starts[i])
    }
}

/// A tree list's chunks of one file (`<hash>:<length>,...`, `:<length>` all zero): the hashes
/// and where each starts; `None` when the lengths do not add up to `size`.
fn parse_chunks(field: &str, size: u64) -> Option<(Vec<Option<Hash>>, Arc<Vec<u64>>)> {
    let (mut hashes, mut starts, mut at) = (Vec::new(), vec![0u64], 0u64);
    for c in field.split(',').filter(|c| !c.is_empty()) {
        let (h, n) = c.split_once(':')?;
        at += n.parse::<u64>().ok().filter(|&n| n > 0)?;
        hashes.push(hex16(h));
        starts.push(at);
    }
    (at == size).then(|| (hashes, Arc::new(starts)))
}

enum Data {
    Dir(BTreeMap<String, Ino>),
    File(Content),
    Link(String),
}

struct Node {
    data: Data,
    /// permission bits only
    mode: u32,
    mtime: i64,
    nlink: u32,
    open_refs: u32,
}

struct Handle {
    ino: Ino,
    pos: u64,
    flags: c_int,
}

#[derive(Default)]
struct Fs {
    nodes: Vec<Option<Node>>,
    mounts: Vec<(String, Ino)>,
    handles: HashMap<u32, Handle>,
    next_handle: u32,
    /// free slots in `nodes`
    free: Vec<Ino>,
    /// what this process has of the processes' shared namespace (vfs/shared.rs)
    #[allow(dead_code)]
    shared: shared::Synced,
    /// the nodes changed since it was last published there
    dirt: shared::Dirt,
}

static FS: Mutex<Option<Fs>> = Mutex::new(None);

/// Records as `Fs::export` makes them.
fn parse_records(mut b: &[u8]) -> Vec<(String, Vec<u8>, i64)> {
    let mut files = Vec::new();
    let u32_at = |b: &[u8]| u32::from_le_bytes(b[..4].try_into().unwrap()) as usize;
    while b.len() >= 4 {
        let n = u32_at(b);
        // (in u64: on wasm32 16 + n wrapped for n near 4 GiB, passed the check and panicked)
        if (b.len() as u64) < 16 + n as u64 {
            break;
        }
        let path = String::from_utf8_lossy(&b[4..4 + n]).into_owned();
        let mtime = i64::from_le_bytes(b[4 + n..12 + n].try_into().unwrap());
        let size = u32_at(&b[12 + n..]);
        if (b.len() as u64) < 16 + n as u64 + size as u64 {
            break;
        }
        files.push((path, b[16 + n..16 + n + size].to_vec(), mtime));
        b = &b[16 + n + size..];
    }
    files
}

/// The page: the files written under each of `roots` (":"-separated paths) as records (see
/// `Fs::export`), in a malloc'ed buffer the caller frees; its length goes to `len`.
/// Bumped by everything that may change the tree (see `vnhost_vfs_changes`).
static CHANGES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// A number that changes whenever the file system may have changed: the page exports the saves
/// only then (an export holds the file system's lock and copies every written file).
#[no_mangle]
pub extern "C" fn vnhost_vfs_changes() -> u32 {
    before_export_run();
    change_count()
}

/// The count `vnhost_vfs_changes` hands the page (without writing what waits first).
pub fn change_count() -> u32 {
    CHANGES.load(std::sync::atomic::Ordering::Relaxed).wrapping_add(shared::generation())
}

/// What writes files it keeps back (sake's registry): run before the page looks for changes
/// and before it exports the saves, so what a game wrote reaches the saves even when the file
/// itself is written later (`before_export`).
static BEFORE_EXPORT: std::sync::Mutex<Vec<fn()>> = std::sync::Mutex::new(Vec::new());

/// Has `f` run before every look of the page at the saves (`vnhost_vfs_changes`,
/// `vnhost_vfs_export`): it writes what it holds back. `f` must not wait for a lock a guest
/// thread may hold long (the page's main thread runs it).
pub fn before_export(f: fn()) {
    let mut l = BEFORE_EXPORT.lock().unwrap_or_else(|e| e.into_inner());
    if !l.iter().any(|&g| g as usize == f as usize) {
        l.push(f);
    }
}

fn before_export_run() {
    let l = BEFORE_EXPORT.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for f in l {
        f();
    }
}

#[no_mangle]
pub unsafe extern "C" fn vnhost_vfs_export(roots: *const c_char, len: *mut size_t) -> *mut u8 {
    before_export_run();
    let roots = CStr::from_ptr(roots).to_string_lossy().into_owned();
    let mut out = Vec::new();
    {
        let mut g = FS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(fs) = g.as_mut() {
            let _shared = shared::Guard::enter(fs);
            for r in roots.split(':').filter(|r| !r.is_empty()) {
                fs.export(&clean(r), &mut out);
            }
        }
    }
    *len = out.len();
    let p = libc::malloc(out.len().max(1)) as *mut u8;
    if !p.is_null() {
        std::ptr::copy_nonoverlapping(out.as_ptr(), p, out.len());
    }
    p
}

fn with<R>(f: impl FnOnce(&mut Fs) -> R) -> R {
    let mut g = FS.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        // Loaded without the lock: loading may wait on the network (a tree list; a dropped
        // mobile link: minutes of retries), and the page's main thread takes the lock too
        // (vnhost_vfs_export), spinning meanwhile. LOADING makes the other threads wait for
        // the one that loads.
        drop(g);
        let _one = LOADING.lock().unwrap_or_else(|e| e.into_inner());
        if FS.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
            let fs = Fs::load();
            *FS.lock().unwrap_or_else(|e| e.into_inner()) = Some(fs);
        }
        g = FS.lock().unwrap_or_else(|e| e.into_inner());
    }
    let fs = g.as_mut().unwrap();
    let shared = shared::Guard::enter(fs);
    let result = f(fs);
    shared.publish(fs);
    result
}

/// Before the first child process starts: from here on the processes share one namespace
/// (each with its own handles and offsets; vfs/shared.rs). A single process never pays for it.
pub fn share() {
    if shared::enable() {
        with(|_| ());
    }
}

static LOADING: Mutex<()> = Mutex::new(());

extern "C" {
    /// host/vnfs.js: reads `name` from the store into a malloc'ed buffer (NULL if missing)
    fn vnvm_fetch(name: *const c_char, len: *mut size_t) -> *mut u8;
    /// host/vnfs.js: store files (newline-separated) the game reads soon: fetched onto the
    /// device by the page, not waited for (see `hint`)
    fn vnvm_fetch_hint(names: *const c_char, len: size_t);
}

// ---------------------------------------------------------------------------------------------
// network counters (the page's debug overlay and the run log's perf lines: vnhost_net_stats)

mod net {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    /// store requests, of them from the browser's HTTP cache, failed
    pub static REQUESTS: AtomicU64 = AtomicU64::new(0);
    pub static CACHED: AtomicU64 = AtomicU64::new(0);
    pub static FAILED: AtomicU64 = AtomicU64::new(0);
    /// bytes over the network (compressed) and after decoding
    pub static WIRE: AtomicU64 = AtomicU64::new(0);
    pub static BYTES: AtomicU64 = AtomicU64::new(0);
    /// time in requests (us, all threads) and the requests on their way now
    pub static REQ_US: AtomicU64 = AtomicU64::new(0);
    pub static INFLIGHT: AtomicU64 = AtomicU64::new(0);
    /// the game's threads waiting for chunks (us), and the longest wait since the last read
    pub static STALL_US: AtomicU64 = AtomicU64::new(0);
    pub static STALL_MAX_US: AtomicU64 = AtomicU64::new(0);
    pub static REQ_MAX_US: AtomicU64 = AtomicU64::new(0);

    pub fn stall(t: std::time::Instant) {
        let us = t.elapsed().as_micros() as u64;
        STALL_US.fetch_add(us, Relaxed);
        STALL_MAX_US.fetch_max(us, Relaxed);
    }
}

/// The network counters into `out` (f64 each): requests, from the HTTP cache, failed, bytes
/// over the network, bytes decoded, request time (ms), requests on their way, the game's
/// threads' wait for chunks (ms), the longest such wait and the longest request since the
/// last call (ms), the chunk cache (bytes), chunks a read fetched again after the cache dropped
/// them.
#[no_mangle]
pub unsafe extern "C" fn vnhost_net_stats(out: *mut f64) {
    use std::sync::atomic::Ordering::Relaxed;
    let ms = |us: u64| us as f64 / 1000.0;
    let (cache, refetched) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map_or((0, 0), |c| (c.bytes, c.refetched));
    let v = [
        net::REQUESTS.load(Relaxed) as f64,
        net::CACHED.load(Relaxed) as f64,
        net::FAILED.load(Relaxed) as f64,
        net::WIRE.load(Relaxed) as f64,
        net::BYTES.load(Relaxed) as f64,
        ms(net::REQ_US.load(Relaxed)),
        net::INFLIGHT.load(Relaxed) as f64,
        ms(net::STALL_US.load(Relaxed)),
        ms(net::STALL_MAX_US.swap(0, Relaxed)),
        ms(net::REQ_MAX_US.swap(0, Relaxed)),
        cache as f64,
        refetched as f64,
    ];
    std::ptr::copy_nonoverlapping(v.as_ptr(), out, v.len());
}

/// Store requests on their way now (the page holds its read-ahead back meanwhile: the game's
/// threads wait for theirs).
#[no_mangle]
pub extern "C" fn vnhost_net_busy() -> u32 {
    net::INFLIGHT.load(std::sync::atomic::Ordering::Relaxed) as u32
}

/// host/vnfs.js: a store request's bytes over the network (its resource timing entry), and
/// whether the browser's HTTP cache had it instead.
#[no_mangle]
pub extern "C" fn vnhost_net_wire(bytes: u32, cached: bool) {
    use std::sync::atomic::Ordering::Relaxed;
    net::WIRE.fetch_add(bytes as u64, Relaxed);
    if cached {
        net::CACHED.fetch_add(1, Relaxed);
    }
}

fn fetch(name: &str) -> Option<Vec<u8>> {
    use std::sync::atomic::Ordering::Relaxed;
    let c = CString::new(name).ok()?;
    let mut len = 0;
    let counted = !name.starts_with('@');
    unsafe {
        let t = std::time::Instant::now();
        if counted {
            net::INFLIGHT.fetch_add(1, Relaxed);
        }
        let p = crate::wrap::load_idle(|| vnvm_fetch(c.as_ptr(), &mut len));
        if counted {
            let us = t.elapsed().as_micros() as u64;
            net::INFLIGHT.fetch_sub(1, Relaxed);
            net::REQUESTS.fetch_add(1, Relaxed);
            net::REQ_US.fetch_add(us, Relaxed);
            net::REQ_MAX_US.fetch_max(us, Relaxed);
            if p.is_null() {
                net::FAILED.fetch_add(1, Relaxed);
            } else {
                net::BYTES.fetch_add(len as u64, Relaxed);
            }
        }
        if trace() {
            eprintln!("vfs: fetch {name}: {}", if p.is_null() { -1 } else { len as isize });
        }
        if p.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(p, len).to_vec();
        libc::free(p as *mut c_void);
        Some(v)
    }
}

fn trace() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("IPPAI_TRACE_VFS").is_some())
}

fn unesc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some(c) => out.push(c),
            None => {}
        }
    }
    out
}

fn hex16(s: &str) -> Option<Hash> {
    if s.len() != 32 {
        return None;
    }
    let mut h = [0; 16];
    for i in 0..16 {
        h[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(h)
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl Fs {
    fn load() -> Fs {
        let mut fs = Fs { next_handle: 1, ..Default::default() };
        let Ok(spec) = std::env::var("IPPAI_MOUNTS") else { return fs };
        let mut mounts: Vec<(String, &str)> =
            spec.split(':').filter_map(|m| m.split_once('=')).map(|(at, name)| (clean(at), name)).collect();
        // "/" first (the page mounts an empty one: then nothing is emscripten's), the others
        // inside it as directories of its tree
        mounts.sort_by_key(|(at, _)| at.len());
        for (at, name) in mounts {
            let root = fs.add(Data::Dir(BTreeMap::new()), 0o755, now());
            if !name.is_empty() {
                match fetch(&format!("trees/{name}.tsv")) {
                    Some(tsv) => fs.load_tree(root, &String::from_utf8_lossy(&tsv)),
                    None => eprintln!("kura: tree {name} not found in the store"),
                }
            }
            if at != "/" {
                if let Some(top) = fs.mount_at("/") {
                    let (dir, leaf) = at.rsplit_once('/').unwrap_or(("", &at));
                    let parent = fs.mkdir_p(top, dir);
                    if let Some(Data::Dir(ch)) = fs.data_mut(parent) {
                        ch.insert(leaf.to_string(), root);
                    }
                } else {
                    // the mount point must exist for whoever lists its parent
                    let _ = std::fs::create_dir_all(&at);
                }
            }
            fs.mounts.push((at, root));
        }
        // saved games the page put back (see web/src/saves.ts)
        if let Some(blob) = fetch("@saves") {
            for (path, data, mtime) in parse_records(&blob) {
                fs.put_file(&path, data, mtime);
            }
        }
        fs
    }

    /// The mount holding `path` (the longest one) and the rest of the path.
    fn split_mount<'a>(&self, path: &'a str) -> Option<(Ino, &'a str)> {
        self.mounts
            .iter()
            .filter(|(at, _)| path == at || path.starts_with(&format!("{}/", at.trim_end_matches('/'))))
            .max_by_key(|(at, _)| at.len())
            .map(|(at, ino)| (*ino, &path[at.len()..]))
    }

    /// Writes a whole file (and its directories) into a mounted tree.
    fn put_file(&mut self, path: &str, data: Vec<u8>, mtime: i64) {
        let Some((mut at, rel)) = self.split_mount(path) else { return };
        let parts: Vec<&str> = rel.split('/').filter(|c| !c.is_empty()).collect();
        let Some((name, dirs)) = parts.split_last() else { return };
        for d in dirs {
            let next = match &self.node(at).map(|n| &n.data) {
                Some(Data::Dir(ch)) => ch.get(*d).copied(),
                _ => return,
            };
            at = match next {
                Some(i) => i,
                None => {
                    let i = self.add(Data::Dir(BTreeMap::new()), 0o755, mtime);
                    if let Some(Data::Dir(ch)) = self.data_mut(at) {
                        ch.insert(d.to_string(), i);
                    }
                    i
                }
            };
        }
        let old = match &self.node(at).map(|n| &n.data) {
            Some(Data::Dir(ch)) => ch.get(*name).copied(),
            _ => return,
        };
        match old.and_then(|i| self.node_mut(i).map(|n| (i, n))) {
            Some((_, n)) if matches!(n.data, Data::File(_)) => {
                n.data = Data::File(Content::Upper(data));
                n.mtime = mtime;
            }
            Some(_) => {}
            None => {
                let i = self.add(Data::File(Content::Upper(data)), 0o644, mtime);
                if let Some(Data::Dir(ch)) = self.data_mut(at) {
                    ch.insert(name.to_string(), i);
                }
            }
        }
    }

    /// The files written here (in memory) under `path`, as records: path length (u32), path,
    /// mtime (i64), size (u32), bytes; little-endian.
    fn export(&self, path: &str, out: &mut Vec<u8>) {
        let Some((at, rel)) = self.split_mount(path) else { return };
        let Some(ino) = self.walk(at, rel) else { return };
        let mut todo = vec![(ino, path.trim_end_matches('/').to_string())];
        while let Some((ino, p)) = todo.pop() {
            let Some(n) = self.node(ino) else { continue };
            match &n.data {
                Data::Dir(ch) => todo.extend(ch.iter().map(|(name, &i)| (i, format!("{p}/{name}")))),
                Data::File(Content::Upper(v)) => {
                    out.extend_from_slice(&(p.len() as u32).to_le_bytes());
                    out.extend_from_slice(p.as_bytes());
                    out.extend_from_slice(&n.mtime.to_le_bytes());
                    out.extend_from_slice(&(v.len() as u32).to_le_bytes());
                    out.extend_from_slice(v);
                }
                _ => {}
            }
        }
    }

    fn load_tree(&mut self, root: Ino, tsv: &str) {
        let mut lines = tsv.lines();
        lines.next();
        for line in lines {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 4 {
                continue;
            }
            let mode = u32::from_str_radix(f[1], 8).unwrap_or(0o644);
            let mtime = f[2].parse().unwrap_or(0);
            let path = unesc(f[3]);
            let (dir, name) = match path.rsplit_once('/') {
                Some((d, n)) => (d, n),
                None => ("", path.as_str()),
            };
            let Some(parent) = self.walk(root, dir) else { continue };
            let data = match f[0] {
                "d" => Data::Dir(BTreeMap::new()),
                "l" if f.len() > 4 => Data::Link(unesc(f[4])),
                "f" if f.len() > 5 => {
                    let size = f[4].parse().unwrap_or(0);
                    let Some((chunks, starts)) = parse_chunks(f[5], size) else { continue };
                    Data::File(Content::Lower { size, chunks: Arc::new(chunks), starts })
                }
                _ => continue,
            };
            let ino = self.add(data, mode, mtime);
            if let Some(Data::Dir(c)) = self.data_mut(parent) {
                c.insert(name.to_string(), ino);
            }
        }
    }

    /// Follows plain directory names (no links) from `from`.
    fn walk(&self, from: Ino, rel: &str) -> Option<Ino> {
        let mut at = from;
        for c in rel.split('/').filter(|c| !c.is_empty()) {
            match &self.node(at)?.data {
                Data::Dir(ch) => at = *ch.get(c)?,
                _ => return None,
            }
        }
        Some(at)
    }

    /// The directory `rel` under `from`, made where missing.
    fn mkdir_p(&mut self, from: Ino, rel: &str) -> Ino {
        let mut at = from;
        for c in rel.split('/').filter(|c| !c.is_empty()) {
            let next = match self.node(at).map(|n| &n.data) {
                Some(Data::Dir(ch)) => ch.get(c).copied(),
                _ => return at,
            };
            at = match next {
                Some(i) => i,
                None => {
                    let i = self.add(Data::Dir(BTreeMap::new()), 0o755, now());
                    if let Some(Data::Dir(ch)) = self.data_mut(at) {
                        ch.insert(c.to_string(), i);
                    }
                    i
                }
            };
        }
        at
    }

    fn add(&mut self, data: Data, mode: u32, mtime: i64) -> Ino {
        CHANGES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let n = Node { nlink: if matches!(data, Data::Dir(_)) { 2 } else { 1 }, data, mode, mtime, open_refs: 0 };
        let i = match self.free.pop() {
            Some(i) => {
                self.nodes[i as usize] = Some(n);
                i
            }
            None => {
                self.nodes.push(Some(n));
                (self.nodes.len() - 1) as Ino
            }
        };
        self.dirt.whole(i);
        i
    }

    fn node(&self, ino: Ino) -> Option<&Node> {
        self.nodes.get(ino as usize)?.as_ref()
    }
    fn node_mut(&mut self, ino: Ino) -> Option<&mut Node> {
        CHANGES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.dirt.whole(ino);
        self.nodes.get_mut(ino as usize)?.as_mut()
    }
    /// A node whose counts, times or mode change, not what it holds (a file in memory's bytes
    /// changed go to `dirt.bytes`): the processes then share only that.
    fn meta_mut(&mut self, ino: Ino) -> Option<&mut Node> {
        CHANGES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.dirt.meta(ino);
        self.nodes.get_mut(ino as usize)?.as_mut()
    }
    fn data_mut(&mut self, ino: Ino) -> Option<&mut Data> {
        Some(&mut self.node_mut(ino)?.data)
    }
    /// What a node holds, for a change that says itself which bytes of a file in memory changed
    /// (`dirt.bytes`) or that it changed only the size (write_at, set_size).
    fn upper_mut(&mut self, ino: Ino) -> Option<&mut Data> {
        CHANGES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(&mut self.nodes.get_mut(ino as usize)?.as_mut()?.data)
    }

    fn mount_at(&self, path: &str) -> Option<Ino> {
        self.mounts.iter().find(|(p, _)| p == path).map(|m| m.1)
    }

    /// Resolves an absolute path.
    fn resolve(&self, path: &str, follow_last: bool) -> Result<Found, c_int> {
        let mut todo: VecDeque<String> = path.split('/').filter(|c| !c.is_empty() && *c != ".").map(String::from).collect();
        let mut cur: Vec<String> = Vec::new();
        let mut node = self.mount_at("/");
        let mut links = 0;
        while let Some(c) = todo.pop_front() {
            let last = todo.is_empty();
            if c == ".." {
                cur.pop();
                node = self.lookup_plain(&cur);
                continue;
            }
            let Some(dir) = node else {
                cur.push(c);
                node = self.mount_at(&join(&cur));
                continue;
            };
            let Some(Node { data: Data::Dir(ch), .. }) = self.node(dir) else { return Err(libc::ENOTDIR) };
            let Some(&child) = ch.get(&c) else {
                if last {
                    return Ok(Found::Missing { parent: dir, name: c });
                }
                return Err(libc::ENOENT);
            };
            if let Some(Node { data: Data::Link(target), .. }) = self.node(child) {
                if !last || follow_last {
                    links += 1;
                    if links > 40 {
                        return Err(libc::ELOOP);
                    }
                    if target.starts_with('/') {
                        cur.clear();
                        node = self.mount_at("/");
                    }
                    for (i, t) in target.split('/').filter(|t| !t.is_empty() && *t != ".").enumerate() {
                        todo.insert(i, t.to_string());
                    }
                    continue;
                }
            }
            cur.push(c);
            node = Some(child);
        }
        let path = join(&cur);
        Ok(match node {
            Some(ino) => Found::Node { ino, path },
            None => Found::Outside(path),
        })
    }

    /// The node at a resolved path (no links in it), if it is ours.
    fn lookup_plain(&self, cur: &[String]) -> Option<Ino> {
        let mut node = self.mount_at("/");
        for i in 0..cur.len() {
            node = match node {
                None => self.mount_at(&join(&cur[..=i])),
                Some(d) => match &self.node(d)?.data {
                    Data::Dir(ch) => Some(*ch.get(&cur[i])?),
                    _ => return None,
                },
            };
        }
        node
    }

    fn file_size(&self, ino: Ino) -> u64 {
        match self.node(ino).map(|n| &n.data) {
            Some(Data::File(Content::Lower { size, .. })) => *size,
            Some(Data::File(Content::Upper(v))) => v.len() as u64,
            _ => 0,
        }
    }

    fn new_handle(&mut self, ino: Ino, flags: c_int) -> u32 {
        let id = self.next_handle;
        self.next_handle += 1;
        self.handles.insert(id, Handle { ino, pos: 0, flags });
        if let Some(Some(n)) = self.nodes.get_mut(ino as usize) {
            n.open_refs += 1;
            self.dirt.refs(ino);
        }
        id
    }

    fn unlink_name(&mut self, parent: Ino, name: &str) {
        let Some(Data::Dir(ch)) = self.data_mut(parent) else { return };
        let Some(ino) = ch.remove(name) else { return };
        if let Some(n) = self.meta_mut(ino) {
            n.nlink = n.nlink.saturating_sub(1);
        }
        self.gc(ino);
    }

    /// Whether `ino` is `dir` or under it.
    fn within(&self, ino: Ino, dir: Ino) -> bool {
        let mut stack = vec![dir];
        while let Some(d) = stack.pop() {
            if d == ino {
                return true;
            }
            if let Some(Data::Dir(ch)) = self.node(d).map(|n| &n.data) {
                stack.extend(ch.values().copied().filter(|&c| matches!(self.node(c).map(|n| &n.data), Some(Data::Dir(_)))));
            }
        }
        false
    }

    /// Frees a node once no name or open handle refers to it.
    fn gc(&mut self, ino: Ino) {
        let linked = self.node(ino).is_some_and(|n| n.nlink > 0 && !(matches!(n.data, Data::Dir(_)) && n.nlink < 2));
        if !linked && self.node(ino).is_none_or(|n| n.open_refs == 0) {
            if let Some(slot @ Some(_)) = self.nodes.get_mut(ino as usize) {
                *slot = None;
                self.free.push(ino);
                self.dirt.whole(ino);
            }
        }
    }
}

enum Found {
    Node { ino: Ino, path: String },
    Missing { parent: Ino, name: String },
    Outside(String),
}

fn join(cur: &[String]) -> String {
    if cur.is_empty() {
        return "/".into();
    }
    let mut s = String::new();
    for c in cur {
        s.push('/');
        s.push_str(c);
    }
    s
}

// ---------------------------------------------------------------------------------------------
// chunk cache

/// Chunks by hash, in two segments (a segmented LRU, like 2Q): a chunk comes in on probation
/// (fetched ahead, or read once) and is protected once read again; protected ones are kept in
/// the order of their last read and hold at most PROTECTED of the limit (the least recent goes
/// back on probation). Over the limit the oldest on probation goes first, then the least recent
/// protected one. A VN reads its scripts, UI pictures and fonts again and again but a voice or
/// a movie once: a long one read through no longer pushes the others out (it did when this was
/// one queue in the order of the puts). `bytes` is the sum of the lengths of what `map` holds.
#[derive(Default)]
struct Cache<K = Key> {
    map: HashMap<K, Entry>,
    /// on probation and protected, by their tick (oldest first)
    probation: BTreeMap<u64, K>,
    protected: BTreeMap<u64, K>,
    bytes: usize,
    protected_bytes: usize,
    tick: u64,
    /// chunks dropped (the latest GONE_MAX or so), and how many reads fetched one again
    gone: std::collections::HashSet<K>,
    refetched: u64,
}

const GONE_MAX: usize = 1 << 16;

struct Entry {
    data: Arc<Vec<u8>>,
    /// reads so far (a chunk fetched ahead has none)
    reads: u32,
    protected: bool,
    tick: u64,
}

/// The share of the limit protected chunks may hold.
const PROTECTED: (usize, usize) = (3, 4);

impl<K: Copy + Eq + std::hash::Hash> Cache<K> {
    /// Holds `data` as `h`, `read` if a read is what fetched it (in place of what `h` held: it
    /// keeps its place), then drops what is over `limit` (`h` itself last).
    fn put(&mut self, h: &K, data: Arc<Vec<u8>>, read: bool, limit: usize) {
        let len = data.len();
        if let Some(e) = self.map.get_mut(h) {
            let had = e.data.len();
            e.data = data;
            self.bytes = self.bytes - had + len;
            if e.protected {
                self.protected_bytes = self.protected_bytes - had + len;
            }
            if read {
                self.read(h);
            }
        } else {
            if read && self.gone.remove(h) {
                self.refetched += 1;
            }
            self.tick += 1;
            self.probation.insert(self.tick, *h);
            self.map.insert(*h, Entry { data, reads: read as u32, protected: false, tick: self.tick });
            self.bytes += len;
        }
        self.trim(limit, h);
    }

    /// A read of `h` (in `map`): protected from the second on, and the most recent.
    fn read(&mut self, h: &K) {
        let Some(e) = self.map.get_mut(h) else { return };
        e.reads = e.reads.saturating_add(1);
        if !e.protected && e.reads < 2 {
            return;
        }
        if e.protected {
            self.protected.remove(&e.tick);
        } else {
            self.probation.remove(&e.tick);
            e.protected = true;
            self.protected_bytes += e.data.len();
        }
        self.tick += 1;
        e.tick = self.tick;
        self.protected.insert(self.tick, *h);
    }

    /// Back on probation (as read once), the most recent there.
    fn demote(&mut self, h: &K) {
        let Some(e) = self.map.get_mut(h) else { return };
        if !e.protected {
            return;
        }
        self.protected.remove(&e.tick);
        self.protected_bytes -= e.data.len();
        e.protected = false;
        e.reads = 1;
        self.tick += 1;
        e.tick = self.tick;
        self.probation.insert(self.tick, *h);
    }

    fn remove(&mut self, h: &K) {
        if let Some(e) = self.map.remove(h) {
            if self.gone.len() >= GONE_MAX {
                self.gone.clear();
            }
            self.gone.insert(*h);
            self.bytes -= e.data.len();
            if e.protected {
                self.protected.remove(&e.tick);
                self.protected_bytes -= e.data.len();
            } else {
                self.probation.remove(&e.tick);
            }
        }
    }

    /// Drops what is over `limit`, `keep` last.
    fn trim(&mut self, limit: usize, keep: &K) {
        while self.protected_bytes > limit / PROTECTED.1 * PROTECTED.0 {
            let Some((_, h)) = self.protected.first_key_value().map(|(t, h)| (*t, *h)) else { break };
            self.demote(&h);
        }
        while self.bytes > limit {
            let first = |m: &BTreeMap<u64, K>| m.values().find(|h| *h != keep).copied();
            let Some(h) = first(&self.probation).or_else(|| first(&self.protected)) else {
                // only `keep` left
                self.remove(&keep.clone());
                break;
            };
            self.remove(&h);
        }
    }

    /// Chunk `h` at `len` bytes (a read): one the page fetched ahead comes as stored (a file's
    /// last chunk short) and is padded in place (a copy only if someone holds it).
    fn get(&mut self, h: &K, len: usize, limit: usize) -> Option<Arc<Vec<u8>>> {
        let e = self.map.get_mut(h)?;
        let had = e.data.len();
        if had != len {
            Arc::make_mut(&mut e.data).resize(len, 0);
            self.bytes = self.bytes - had + len;
            if e.protected {
                self.protected_bytes = self.protected_bytes - had + len;
            }
        }
        let d = e.data.clone();
        self.read(h);
        self.trim(limit, h);
        Some(d)
    }

    fn contains(&self, h: &K) -> bool {
        self.map.contains_key(h)
    }
}

static CACHE: Mutex<Option<Cache<Key>>> = Mutex::new(None);

fn cache_limit() -> usize {
    static LIMIT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *LIMIT.get_or_init(|| std::env::var("IPPAI_CHUNK_CACHE_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(128usize) << 20)
}

/// The chunks this run read, in the order of their first read (the first FETCHED_MAX): the
/// next run's page fetches them ahead, several at a time (see jit::maybe_save). Here each is
/// one synchronous request. (Chunks the page fetched ahead count too: else a run that had
/// all it read fetched ahead would leave the next an empty list.)
static FETCHED: Mutex<Option<(Vec<Hash>, std::collections::HashSet<Hash>)>> = Mutex::new(None);
const FETCHED_MAX: usize = 8192;

/// [`FETCHED`] as concatenated hashes.
pub fn fetched() -> Vec<u8> {
    FETCHED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map_or(Vec::new(), |f| f.0.concat())
}

fn note_read(h: &Hash) {
    let mut g = FETCHED.lock().unwrap_or_else(|e| e.into_inner());
    let (list, seen) = g.get_or_insert_with(Default::default);
    if list.len() < FETCHED_MAX && seen.insert(*h) {
        list.push(*h);
    }
}

// ---------------------------------------------------------------------------------------------
// blocks: what is cached here and read from the device's copy is a BLOCK of a chunk, not the
// chunk. A chunk (FastCDC, 4 MiB on average) is the store's unit: one S3 object, one request
// over the network, one Cache Storage entry. Reading a whole chunk for every 64 KiB the game
// asks for made big chunks slow once they were on the device (docs/chunk-size.md: a 16 MiB
// chunk read from Cache Storage, copied three times and decoded for 64 KiB): Cache Storage's
// Blob.slice reads only the bytes asked for (measured in Chromium: ~1.3 ms for 64 or 256 KiB
// of a 1, 4, 16 or 64 MiB entry; the whole entry ~1.4 ms per MiB), so a read now fetches only
// its blocks (host/fetch-bridge.js reads them out of the chunk; a chunk not on the device yet is
// fetched whole once and kept, its blocks then come from there).

/// A block of a chunk: the chunk and the block's number in it.
type Key = (Hash, u32);

/// Bytes in a block (IPPAI_VFS_BLOCK_KIB, default 256): small enough that a scattered read
/// fetches little more than it needs, big enough that a long read is not a request per few KiB.
/// (Tests: 24 bytes, so their small files have many blocks and blocks that are not a power of two.)
fn block() -> u64 {
    #[cfg(test)]
    {
        24
    }
    #[cfg(not(test))]
    {
        static B: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        *B.get_or_init(|| std::env::var("IPPAI_VFS_BLOCK_KIB").ok().and_then(|v| v.parse::<u64>().ok()).filter(|&k| k > 0).unwrap_or(256) << 10)
    }
}

/// Blocks fetched in one request at most (by a fetcher thread, or by a read for its own first
/// ones): 1 MiB at 256 KiB.
const PIECE: u32 = 4;
/// Blocks fetched ahead past a read's end: SEQ_AHEAD when it follows the file's last read (Godot
/// reads its .pck front to back in small reads; a movie), else one.
const SEQ_AHEAD: u64 = 32;
const NEXT_AHEAD: u64 = 1;

/// Block `b`'s length in a chunk of `clen` bytes.
fn block_len(clen: u64, b: u32) -> usize {
    (clen - b as u64 * block()).min(block()) as usize
}

/// Blocks `first..first + n` of chunk `h` (`clen` bytes long): fetched in one request.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Piece {
    h: Hash,
    clen: u64,
    first: u32,
    n: u32,
}

/// The pieces to fetch for a read of `off..end` of a file laid out as `l` with chunks
/// `chunks`, and ahead of it to `ahead_end`: every block of a stored (not all-zero) chunk in
/// `off..ahead_end`, in pieces of at most PIECE blocks of one chunk, in file order. With `own`,
/// the read's first piece is left out (the reading thread fetches it itself while the fetcher
/// threads fetch the rest).
fn pieces(off: u64, end: u64, ahead_end: u64, l: &Layout, chunks: &[Option<Hash>], own: bool) -> Vec<Piece> {
    let b = block();
    let mut out = Vec::new();
    if off >= ahead_end {
        return out;
    }
    let (first, last) = (l.index(off), l.index(ahead_end - 1));
    // (the read's first piece: in its first chunk, unless that one is all zero)
    let mut skip = own && off < end && chunks.get(first).copied().flatten().is_some();
    for i in first..=last.min(chunks.len().saturating_sub(1)) {
        let (cstart, clen) = l.span(i);
        let from = off.max(cstart) - cstart;
        let to = ahead_end.min(cstart + clen) - cstart;
        let Some(h) = chunks[i] else { continue };
        let (b0, b1) = ((from / b) as u32, ((to - 1) / b) as u32);
        let mut at = b0;
        if skip {
            skip = false;
            at += own_blocks(off, end, l) as u32;
        }
        while at <= b1 {
            let n = PIECE.min(b1 + 1 - at);
            out.push(Piece { h, clen, first: at, n });
            at += n;
        }
    }
    out
}

/// The read's own first piece (pieces with `own`): this many blocks from `off`'s, the read's
/// blocks in its first chunk, PIECE at most.
fn own_blocks(off: u64, end: u64, l: &Layout) -> u64 {
    let b = block();
    let i = l.index(off);
    let (cstart, clen) = l.span(i);
    let last = (end.min(cstart + clen) - 1 - cstart) / b;
    (last + 1 - (off - cstart) / b).min(PIECE as u64)
}

/// Bytes `from..from + len` of chunk `h` (decoded; fewer when the stored chunk ends short: its
/// zero tail is cut). `from` is a block's start (host/fetch-bridge.js keeps what came over the
/// network as blocks).
fn fetch_range(h: &Hash, from: u64, len: u64, ahead: bool) -> Option<Vec<u8>> {
    let hex = hex(h);
    // ("!a": a fetcher thread's, ahead of the game's reads: the page's scheduler serves the
    // game's first)
    fetch(&format!("chunks/{}/{hex}#{from}+{len}/{}{}", &hex[..2], block(), if ahead { "!a" } else { "" }))
}

fn hex(h: &Hash) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

/// `data`, bytes from block `first` of chunk `h` on, as blocks (each padded to its length);
/// kept when `keep(block)` says so (`read`: a read fetched them, as cache_put).
fn put_blocks(h: &Hash, clen: u64, first: u32, n: u32, data: Vec<u8>, read: bool, keep: impl Fn(u32) -> bool) -> Vec<Arc<Vec<u8>>> {
    let b = block() as usize;
    let mut out = Vec::with_capacity(n as usize);
    if n == 1 {
        let mut data = data;
        data.resize(block_len(clen, first), 0);
        out.push(Arc::new(data));
    } else {
        for k in 0..n {
            let len = block_len(clen, first + k);
            let at = (k as usize * b).min(data.len());
            let mut v = data[at..(at + len).min(data.len())].to_vec();
            v.resize(len, 0);
            out.push(Arc::new(v));
        }
    }
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let c = g.get_or_insert_with(Cache::default);
    for (k, d) in out.iter().enumerate() {
        let key = (*h, first + k as u32);
        if keep(key.1) {
            c.put(&key, d.clone(), read, cache_limit());
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// fetching ahead: a read of several pieces (a CG or a voice read whole) used to fetch them one
// synchronous request after another on the game's thread; the ones after the first now go to
// a few fetcher threads at once, and so do the blocks after the read.

const FETCHERS: usize = 3;

struct Ahead {
    queue: VecDeque<Piece>,
    /// blocks queued or being fetched
    busy: std::collections::HashSet<Key>,
    /// fetcher threads running (none when they could not be made: a thread limit, little
    /// memory; tried again at the next read)
    threads: usize,
}

#[cfg(test)]
static SPAWN_FAILS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// (tests: fetcher threads registered with snap)
#[cfg(test)]
static FETCHERS_UP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn spawn_fetcher(i: usize) -> std::io::Result<()> {
    #[cfg(test)]
    if SPAWN_FAILS.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(std::io::Error::other("no threads (test)"));
    }
    std::thread::Builder::new().name(format!("vfs fetch {i}")).spawn(fetcher).map(drop)
}

static AHEAD: Mutex<Option<Ahead>> = Mutex::new(None);
static AHEAD_WORK: std::sync::Condvar = std::sync::Condvar::new();
static AHEAD_DONE: std::sync::Condvar = std::sync::Condvar::new();

#[cfg(test)]
fn needs_fetchers() -> bool {
    AHEAD.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_none_or(|a| a.threads == 0)
}

/// Has piece `p`'s blocks fetched by the fetcher threads, but those here already or on their
/// way (the rest in runs, a piece each). With no fetcher thread (none could be made) nothing
/// is queued: the read fetches them itself, as before.
fn fetch_ahead(p: Piece) {
    let have: Vec<bool> = {
        let g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        (p.first..p.first + p.n).map(|b| g.as_ref().is_some_and(|c| c.contains(&(p.h, b)))).collect()
    };
    if have.iter().all(|&x| x) {
        return;
    }
    // (tests: the fetchers come up outside the snap tests' pauses; ONE before AHEAD, which
    // fetchers wait for)
    #[cfg(test)]
    let starting = needs_fetchers();
    // Release AHEAD before waiting for ONE. In one chained expression its temporary guard
    // lived through then's closure: a snap test held ONE while its fetchers needed AHEAD.
    #[cfg(test)]
    let _one = starting.then(|| crate::snap::ONE.lock().unwrap_or_else(|e| e.into_inner()));
    let mut g = AHEAD.lock().unwrap_or_else(|e| e.into_inner());
    let a = g.get_or_insert_with(|| Ahead { queue: VecDeque::new(), busy: Default::default(), threads: 0 });
    if a.threads == 0 {
        #[cfg(test)]
        let up = FETCHERS_UP.load(std::sync::atomic::Ordering::SeqCst);
        a.threads = (0..FETCHERS).filter(|&i| spawn_fetcher(i).is_ok()).count();
        #[cfg(test)]
        while FETCHERS_UP.load(std::sync::atomic::Ordering::SeqCst) < up + a.threads {
            std::thread::yield_now();
        }
        if a.threads == 0 {
            return;
        }
    }
    let mut run: Option<Piece> = None;
    for (k, b) in (p.first..p.first + p.n).enumerate() {
        let new = !have[k] && a.busy.insert((p.h, b));
        match (&mut run, new) {
            (Some(r), true) => r.n += 1,
            (None, true) => run = Some(Piece { first: b, n: 1, ..p }),
            (Some(_), false) => a.queue.push_back(run.take().unwrap()),
            (None, false) => {}
        }
    }
    a.queue.extend(run);
    AHEAD_WORK.notify_all();
}

/// A restored save state: no fetcher of the old module is left (the next fetch ahead starts
/// them again; what they had queued is fetched when read).
pub(crate) fn snap_restore() {
    *AHEAD.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// A state is being taken: the fetchers stand still (crate::snap::busy).
pub(crate) fn snap_wake() {
    // (under AHEAD: a fetcher that saw no pause under it is in its wait by now, so this wakes
    // it; it waits without a short timeout for this)
    drop(AHEAD.lock().unwrap_or_else(|e| e.into_inner()));
    AHEAD_WORK.notify_all();
}

fn fetcher() {
    crate::snap::register("vfs fetcher", false, 0);
    #[cfg(test)]
    FETCHERS_UP.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    loop {
        let p = loop {
            crate::snap::idle();
            let mut g = AHEAD.lock().unwrap_or_else(|e| e.into_inner());
            let got = loop {
                if crate::snap::pausing() {
                    break None;
                }
                if let Some(p) = g.as_mut().and_then(|a| a.queue.pop_front()) {
                    break Some(p);
                }
                // (woken for work and for a pause, both under AHEAD: the timeout only a backstop;
                // a 100 ms one woke three threads 30 times a second on an idle game)
                g = AHEAD_WORK.wait_timeout(g, FETCHER_IDLE).unwrap_or_else(|e| e.into_inner()).0;
            };
            drop(g);
            // (while a state is taken: stands still there, the lock let go)
            crate::snap::busy();
            if let Some(p) = got {
                break p;
            }
        };
        fetch_piece(&p, true);
    }
}

/// How long an idle fetcher waits before it looks again by itself.
const FETCHER_IDLE: std::time::Duration = std::time::Duration::from_secs(5);

/// Fetches a queued piece's blocks into the cache (`ahead`: at the page's low priority) and
/// says they are no longer busy.
fn fetch_piece(p: &Piece, ahead: bool) {
    let b = block();
    let from = p.first as u64 * b;
    let len = (p.clen - from).min(p.n as u64 * b);
    if let Some(data) = fetch_range(&p.h, from, len, ahead) {
        put_blocks(&p.h, p.clen, p.first, p.n, data, false, |_| true);
    }
    if let Some(a) = AHEAD.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        for k in p.first..p.first + p.n {
            a.busy.remove(&(p.h, k));
        }
    }
    AHEAD_DONE.notify_all();
}

/// Waits while a fetcher has block `k` on its way. A block still queued (no fetcher took it
/// yet) is taken out of the queue and fetched here, at the read's priority: a read waited
/// behind every piece queued before it (another file's read-ahead: a movie's 8 MiB), and,
/// while a save state had the fetchers stand still, for ever (the save failed: the reading
/// thread could not stop).
fn wait_ahead(k: &Key) {
    let mut g = AHEAD.lock().unwrap_or_else(|e| e.into_inner());
    while g.as_ref().is_some_and(|a| a.busy.contains(k)) {
        let a = g.as_mut().unwrap();
        if let Some(i) = a.queue.iter().position(|p| p.h == k.0 && p.first <= k.1 && k.1 < p.first + p.n) {
            let p = a.queue.remove(i).unwrap();
            drop(g);
            #[cfg(test)]
            TAKEN_BACK.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            fetch_piece(&p, false);
            g = AHEAD.lock().unwrap_or_else(|e| e.into_inner());
            continue;
        }
        g = AHEAD_DONE.wait(g).unwrap_or_else(|e| e.into_inner());
    }
}

/// (tests: queued pieces a read fetched itself)
#[cfg(test)]
static TAKEN_BACK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Blocks `b0..=b1` of chunk `h` (`clen` bytes long) for a read: from the cache, else (each run
/// of missing ones) in one request.
fn chunk_blocks(h: &Hash, clen: u64, b0: u32, b1: u32) -> Result<Vec<Arc<Vec<u8>>>, c_int> {
    let t = std::time::Instant::now();
    let r = chunk_blocks_wait(h, clen, b0, b1);
    if r.is_ok() {
        note_read(h);
    }
    // (blocks in the cache return in a few us: only real waits count)
    if t.elapsed().as_micros() > 200 {
        net::stall(t);
    }
    r
}

fn chunk_blocks_wait(h: &Hash, clen: u64, b0: u32, b1: u32) -> Result<Vec<Arc<Vec<u8>>>, c_int> {
    let busy = |b: u32| AHEAD.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|a| a.busy.contains(&(*h, b)));
    let get = |b: u32| CACHE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Cache::default).get(&(*h, b), block_len(clen, b), cache_limit());
    let mut out = Vec::with_capacity((b1 + 1 - b0) as usize);
    let mut b = b0;
    while b <= b1 {
        // (a block a fetcher has: waited for; the ones no fetcher has: fetched here, a run of
        // them in one request, while the fetchers fetch theirs)
        if busy(b) {
            wait_ahead(&(*h, b));
        }
        if let Some(d) = get(b) {
            out.push(d);
            b += 1;
            continue;
        }
        let s = b;
        b += 1;
        while b <= b1 && !busy(b) && !cached(&(*h, b)) {
            b += 1;
        }
        let from = s as u64 * block();
        let len = (clen - from).min((b - s) as u64 * block());
        let data = fetch_range(h, from, len, false).ok_or(libc::EIO)?;
        out.extend(put_blocks(h, clen, s, b - s, data, true, |_| true));
    }
    Ok(out)
}

fn cached(k: &Key) -> bool {
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|c| c.contains(k))
}

/// A chunk the page fetched ahead: `hash` (16 bytes) and the `data` it malloc'ed, which this
/// takes (the system allocator is malloc), kept as blocks (the chunk's length is not known
/// here: the last block as stored, padded when read).
#[no_mangle]
pub unsafe extern "C" fn vnhost_vfs_put_chunk(hash: *const u8, data: *mut u8, len: size_t) {
    let h: Hash = std::slice::from_raw_parts(hash, 16).try_into().unwrap();
    if len == 0 {
        // (a Vec of capacity 0 never frees: the page's malloc'ed block leaked per empty chunk)
        libc::free(data as *mut libc::c_void);
        return;
    }
    let data = Vec::from_raw_parts(data, len, len);
    let n = (len as u64).div_ceil(block()) as u32;
    if n > 0 {
        put_blocks(&h, len as u64, 0, n, data, false, |_| true);
    }
}

// ---------------------------------------------------------------------------------------------
// hints: when a read waits for the network, what the game reads soon goes to the page to fetch
// onto the device meanwhile (vfs/hints.rs says what, by how engines lay out their files; the
// page's network worker fetches them below the game's own reads, host/fetch-bridge.js
// ippaiHint).

/// Store files' paths by inode, as they were opened (a read knows only the inode): the folder
/// their neighbours are in.
static NAMES: Mutex<Option<HashMap<Ino, String>>> = Mutex::new(None);
/// (inode, chunk) pairs hinted already, and (inode, u32::MAX): files whose neighbours were.
static HINTED: Mutex<Option<std::collections::HashSet<(Ino, u32)>>> = Mutex::new(None);
const HINTED_MAX: usize = 1 << 16;

fn note_name(ino: Ino, path: &str) {
    let mut g = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::default);
    if m.len() > 1 << 16 {
        m.clear();
    }
    m.insert(ino, path.to_string());
    drop(g);
    trace::name(ino, path);
}

/// Whether (`ino`, `what`) is new (u32::MAX: the file's neighbours).
fn first_hint(ino: Ino, what: u32) -> bool {
    let mut g = HINTED.lock().unwrap_or_else(|e| e.into_inner());
    let s = g.get_or_insert_with(Default::default);
    if s.len() > HINTED_MAX {
        s.clear();
    }
    s.insert((ino, what))
}

/// The store chunks a read of chunk `i` of file `ino` (laid out as `l`, `chunks`) says the game
/// reads soon (`first`: the file's first read): see vfs/hints.rs. Not those in the cache.
fn hinted(ino: Ino, l: &Layout, chunks: &[Option<Hash>], i: usize, first: bool) -> Vec<Hash> {
    let path = NAMES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&ino).cloned());
    let Some(path) = path else { return Vec::new() };
    let name = path.rsplit('/').next().unwrap_or(&path);
    let mut out = Vec::new();
    // (each chunk once: not the one being read, nor one hinted before)
    for k in hints::chunks(name, l.size, chunks.len(), i, first) {
        if k != i && first_hint(ino, k as u32) {
            out.extend(chunks[k]);
        }
    }
    if first && first_hint(ino, u32::MAX) {
        let dir = path.rsplit_once('/').map_or("/", |(d, _)| if d.is_empty() { "/" } else { d });
        let near: Vec<Hash> = with(|fs| {
            let Ok(Found::Node { ino: d, .. }) = fs.resolve(dir, true) else { return Vec::new() };
            let Some(Data::Dir(ch)) = fs.node(d).map(|n| &n.data) else { return Vec::new() };
            let lower = |f: Ino| match fs.node(f).map(|n| &n.data) {
                Some(Data::File(Content::Lower { size, chunks, .. })) => Some((*size, chunks)),
                _ => None,
            };
            // (the folder is sorted by name: the files after this one, as far as the hints
            // go; not a list of every entry per first read of each loose file)
            if ch.get(name).and_then(|&f| lower(f)).is_none() {
                return Vec::new();
            }
            let after = ch.range::<str, _>((std::ops::Bound::Excluded(name), std::ops::Bound::Unbounded)).filter_map(|(n, &f)| lower(f).map(|(size, c)| (n.as_str(), size, c)));
            hints::siblings_after(after, name, hints::policy(name)).into_iter().flat_map(|c| c.iter().flatten().copied()).collect()
        });
        out.extend(near);
    }
    let g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    out.retain(|h| g.as_ref().is_none_or(|c| !c.contains(&(*h, 0))));
    out
}

/// Tells the page what a read of chunk `i` of `ino` says the game reads soon (`hinted`).
fn hint(ino: Ino, l: &Layout, chunks: &[Option<Hash>], i: usize, first: bool) {
    send_hint(&hinted(ino, l, chunks, i, first), "layout");
}

/// Tells the page the chunks `hs`, to fetch onto the device (host/fetch-bridge.js ippaiHint);
/// `source` says which foresight gave them (the run's trace keeps it: vfs/trace.rs).
fn send_hint(hs: &[Hash], source: &str) {
    if hs.is_empty() {
        return;
    }
    trace::hint(source, hs);
    let names: Vec<String> = hs.iter().map(|h| {
        let x = hex(h);
        format!("chunks/{}/{x}", &x[..2])
    }).collect();
    let s = names.join("\n");
    if trace() {
        eprintln!("vfs: hint {} chunks", names.len());
    }
    unsafe { vnvm_fetch_hint(s.as_ptr() as *const c_char, s.len()) };
}

// ---------------------------------------------------------------------------------------------
// predictions: a script the game reads names what it shows and plays next (vfs/predict.rs);
// those files go to the page to fetch onto the device, as hints do.

/// What a name in a script points at: a store file, or an archive's member (the archive, its
/// bytes' ranges in it).
#[derive(Clone)]
enum Target {
    File(Ino),
    Member(Ino, Vec<(u64, u64)>),
}

#[derive(Default)]
struct Predict {
    /// Lower-case names (and names without their extension) -> what they are.
    names: HashMap<String, Vec<Target>>,
    /// Whether the store's files are in `names` yet (at the first script read).
    files: bool,
    /// Archives looked at: their script members (name, segments), by where they start.
    archives: HashMap<Ino, Vec<predict::Member>>,
    /// Scripts looked into: (file, member start; 0 for a loose one).
    scanned: std::collections::HashSet<(Ino, u64)>,
    /// The trees' scripts and what they name, as ippai serve read them all ahead
    /// (vnhost_vfs_foresee; crates/ippai/src/foresee.rs), and those keys without extension.
    table: HashMap<String, Vec<String>>,
    stems: HashMap<String, String>,
    /// Where the table's scripts name each name (foresee::table_places).
    places: HashMap<String, Vec<(String, usize)>>,
}

/// Whether a table came (then every file's first read asks what follows it).
static HAS_TABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Names after a file in each script that names it, and in how many scripts at most a name may
/// stand to say where the game is.
const FOLLOWING: usize = 12;
const PLACES_COMMON: usize = 4;

/// The archives looked into.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pack {
    Xp3,
    Bgi,
}

/// A table's levels of scripts named by a script read (its own names, then the next scripts').
const FORESEE_DEPTH: usize = 1;

/// The page hands a tree's table (text, `len` bytes at `text`: foresee::parse_table), read by
/// ippai serve from all its scripts ahead: a script read then foresees what it names and what
/// the scripts it goes on to name, without reading it here (scripts this cannot read, too).
#[no_mangle]
pub unsafe extern "C" fn vnhost_vfs_foresee(text: *const u8, len: size_t) {
    let text = String::from_utf8_lossy(std::slice::from_raw_parts(text, len)).into_owned();
    foresee_table(&text);
}

fn foresee_table(text: &str) {
    let t = predict::parse_table(text);
    let mut g = PREDICT.lock().unwrap_or_else(|e| e.into_inner());
    let p = g.get_or_insert_with(Predict::default);
    for (k, v) in t {
        let e = p.table.entry(k).or_default();
        for n in v {
            if !e.contains(&n) {
                e.push(n);
            }
        }
    }
    p.stems = predict::table_stems(&p.table);
    p.places = predict::table_places(&p.table, PLACES_COMMON);
    HAS_TABLE.store(!p.table.is_empty(), std::sync::atomic::Ordering::Relaxed);
}
static PREDICT: Mutex<Option<Predict>> = Mutex::new(None);
/// Bytes a script's names fetch ahead at most, and each file or member at most.
const PREDICT_BYTES: u64 = 48 << 20;
std::thread_local! {
    /// In a prediction's own reads (read_at again: no prediction of those).
    static PREDICTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn add_name(names: &mut HashMap<String, Vec<Target>>, name: &str, t: Target) {
    let (full, stem) = predict::keys(name.rsplit(['/', '\\']).next().unwrap_or(name));
    for k in std::iter::once(full).chain(stem) {
        let v = names.entry(k).or_default();
        if v.len() < 8 {
            v.push(t.clone());
        }
    }
}

/// The bytes `off..off + len` of `ino` (a prediction's own read: short at the end).
fn read_bytes(ino: Ino, off: u64, len: u64) -> Option<Vec<u8>> {
    let mut b = vec![0u8; len as usize];
    let n = read_at(ino, &mut b, off).ok()?;
    b.truncate(n);
    Some(b)
}

/// An .xp3's members, from its index (read here, as the game reads it at its start).
fn xp3_members(ino: Ino) -> Vec<predict::Member> {
    let Some(head) = read_bytes(ino, 0, 0x28) else { return Vec::new() };
    let Some(at) = predict::xp3_index_at(&head) else { return Vec::new() };
    let Some(len) = read_bytes(ino, at, 17).as_deref().and_then(predict::xp3_index_len) else { return Vec::new() };
    if len > predict::INDEX_MAX as u64 {
        return Vec::new();
    }
    read_bytes(ino, at, len).as_deref().and_then(predict::xp3_index_bytes).map(|i| predict::xp3_index(&i)).unwrap_or_default()
}

/// A BGI .arc's members, from its index (read here, as the game reads it at its start).
fn bgi_members(ino: Ino) -> Vec<predict::Member> {
    let Some(len) = read_bytes(ino, 0, 16).as_deref().and_then(predict::bgi_index_len) else { return Vec::new() };
    if len > predict::INDEX_MAX as u64 {
        return Vec::new();
    }
    read_bytes(ino, 0, len).map(|h| predict::bgi_index(&h)).unwrap_or_default()
}

/// After a read of `ino` at `off..off + len` (the game's): a script read the first time is
/// looked into, and what it names goes to the page as hints.
fn predict_read(ino: Ino, off: u64, len: u64) {
    if PREDICTING.with(|p| p.replace(true)) {
        return;
    }
    let hs = predicted(ino, off, len);
    PREDICTING.with(|p| p.set(false));
    if trace() && !hs.is_empty() {
        eprintln!("vfs: a script read names {} chunks", hs.len());
    }
    send_hint(&hs, "script");
}

/// The chunks scripts reached by this read name (`predict_read`), those not on the device, in the
/// order named, PREDICT_BYTES at most.
fn predicted(ino: Ino, off: u64, len: u64) -> Vec<Hash> {
    // (every read comes here: the name looked at in place, nothing copied for most)
    let (archive, script, key) = {
        let g = NAMES.lock().unwrap_or_else(|e| e.into_inner());
        let Some(path) = g.as_ref().and_then(|m| m.get(&ino)) else { return Vec::new() };
        let name = path.rsplit('/').next().unwrap_or(path);
        let ext = |e: &[u8]| name.len() > 4 && name.as_bytes()[name.len() - 4..].eq_ignore_ascii_case(e);
        let archive = if ext(b".xp3") { Some(Pack::Xp3) } else if ext(b".arc") { Some(Pack::Bgi) } else { None };
        let script = predict::is_script(name);
        // (another file: only when a table says what follows it)
        if archive.is_none() && !script && !HAS_TABLE.load(std::sync::atomic::Ordering::Relaxed) {
            return Vec::new();
        }
        (archive, script, predict::script_key(name))
    };
    // the store's files by name, once (the trees do not change)
    let (files, first) = {
        let mut g = PREDICT.lock().unwrap_or_else(|e| e.into_inner());
        let p = g.get_or_insert_with(Predict::default);
        (std::mem::replace(&mut p.files, true), !p.archives.contains_key(&ino))
    };
    if !files {
        let mut names = HashMap::new();
        with(|fs| {
            for n in fs.nodes.iter().flatten() {
                if let Data::Dir(ch) = &n.data {
                    for (nm, &f) in ch {
                        if matches!(fs.node(f).map(|x| &x.data), Some(Data::File(Content::Lower { .. }))) {
                            add_name(&mut names, nm, Target::File(f));
                        }
                    }
                }
            }
        });
        let mut g = PREDICT.lock().unwrap_or_else(|e| e.into_inner());
        let p = g.get_or_insert_with(Predict::default);
        for (k, v) in names {
            p.names.entry(k).or_default().extend(v);
        }
    }
    // what the scripts this read reaches name: from the table when it has them, else read here;
    // what follows a file that is no script, by where the table's scripts name it
    let from_table = |key: &str| -> Option<Vec<String>> {
        let g = PREDICT.lock().unwrap_or_else(|e| e.into_inner());
        let p = g.as_ref()?;
        p.table.contains_key(key).then(|| predict::foreseen(&p.table, &p.stems, key, FORESEE_DEPTH, predict::MENTIONS_MAX))
    };
    let after = |key: &str| -> Vec<String> {
        let g = PREDICT.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref().map_or(Vec::new(), |p| predict::following(&p.table, &p.places, key, FOLLOWING))
    };
    let mut lists: Vec<Vec<String>> = Vec::new();
    if let Some(kind) = archive {
        if first {
            let members = match kind {
                Pack::Xp3 => xp3_members(ino),
                Pack::Bgi => bgi_members(ino),
            };
            let mut g = PREDICT.lock().unwrap_or_else(|e| e.into_inner());
            let p = g.get_or_insert_with(Predict::default);
            let mut all = Vec::with_capacity(members.len());
            for m in members {
                add_name(&mut p.names, &m.name, Target::Member(ino, m.segments.iter().map(|s| (s.0, s.1)).collect()));
                all.push(m);
            }
            all.sort_by_key(|m| m.segments[0].0);
            p.archives.insert(ino, all);
        }
        let reached: Vec<predict::Member> = {
            let mut g = PREDICT.lock().unwrap_or_else(|e| e.into_inner());
            let p = g.get_or_insert_with(Predict::default);
            // (by where they start: those before the read's end, back to one that ends before it)
            let ms: Vec<predict::Member> = p.archives.get(&ino).map_or(Vec::new(), |ms| {
                let to = ms.partition_point(|m| m.segments[0].0 < off + len.max(1));
                ms[..to].iter().rev().take(64).filter(|m| m.segments.iter().any(|s| s.0 < off + len.max(1) && off < s.0 + s.1)).cloned().collect()
            });
            ms.into_iter().filter(|m| p.scanned.insert((ino, m.segments[0].0))).collect()
        };
        for m in reached {
            let k = predict::script_key(&m.name);
            if let Some(l) = from_table(&k) {
                lists.push(l);
                continue;
            }
            if !predict::is_script(&m.name) || kind != Pack::Xp3 {
                lists.push(after(&k));
                continue;
            }
            if m.segments.iter().map(|s| s.1).sum::<u64>() > predict::SCRIPT_MAX as u64 {
                continue;
            }
            let parts: Option<Vec<(Vec<u8>, bool)>> = m.segments.iter().map(|s| read_bytes(ino, s.0, s.1).map(|b| (b, s.2))).collect();
            let Some(parts) = parts else { continue };
            let refs: Vec<(&[u8], bool)> = parts.iter().map(|(b, z)| (&b[..], *z)).collect();
            lists.extend(predict::member_bytes(&refs).map(|b| predict::names_in(&b)));
        }
    } else {
        let fresh = PREDICT.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Predict::default).scanned.insert((ino, 0));
        if fresh {
            if let Some(l) = from_table(&key) {
                lists.push(l);
            } else if script {
                let size = with(|fs| match fs.node(ino).map(|n| &n.data) {
                    Some(Data::File(Content::Lower { size, .. })) => *size,
                    _ => 0,
                });
                if size <= predict::SCRIPT_MAX as u64 {
                    lists.extend(read_bytes(ino, 0, size).map(|b| predict::names_in(&b)));
                }
            } else {
                lists.push(after(&key));
            }
        }
    }
    // what the scripts name -> the chunks of it, in the order named
    let mut out = Vec::new();
    let mut bytes = 0u64;
    let mut seen = std::collections::HashSet::new();
    for names in lists {
        let targets: Vec<Target> = {
            let g = PREDICT.lock().unwrap_or_else(|e| e.into_inner());
            let Some(p) = g.as_ref() else { break };
            names.iter().flat_map(|n| {
                let (full, stem) = predict::keys(n);
                p.names.get(&full).or_else(|| stem.and_then(|s| p.names.get(&s))).cloned().unwrap_or_default()
            }).collect()
        };
        for t in targets {
            // (its chunks, and its bytes: nothing past SIBLING_MAX, a voice or a picture)
            let (hs, n): (Vec<Hash>, u64) = with(|fs| {
                let lower = |f: Ino| match fs.node(f).map(|n| &n.data) {
                    Some(Data::File(Content::Lower { size, chunks, starts })) => Some((Layout { size: *size, starts: starts.clone() }, chunks.clone())),
                    _ => None,
                };
                match &t {
                    Target::File(f) => match lower(*f) {
                        Some((l, c)) if l.size <= hints::SIBLING_MAX => (c.iter().flatten().copied().collect(), l.size),
                        _ => (Vec::new(), 0),
                    },
                    Target::Member(a, ranges) => {
                        let n: u64 = ranges.iter().map(|r| r.1).sum();
                        let Some((l, c)) = lower(*a).filter(|_| n <= hints::SIBLING_MAX) else { return (Vec::new(), 0) };
                        let mut v = Vec::new();
                        for &(s, n) in ranges {
                            if n == 0 || s >= l.size {
                                continue;
                            }
                            for i in l.index(s)..=l.index((s + n - 1).min(l.size - 1)) {
                                v.extend(c.get(i).copied().flatten());
                            }
                        }
                        (v, n)
                    }
                }
            });
            if hs.is_empty() {
                continue;
            }
            if bytes + n > PREDICT_BYTES {
                break;
            }
            bytes += n;
            out.extend(hs.into_iter().filter(|h| seen.insert(*h)));
        }
    }
    let g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    out.retain(|h| g.as_ref().is_none_or(|c| !c.contains(&(*h, 0))));
    out
}

/// Per file: where its last read ended (a read that starts there reads on: SEQ_AHEAD blocks
/// are fetched ahead, not one), and the bytes whose blocks reads have queued to be fetched
/// already (`ahead`: a read whose window lies in them queues nothing; small reads one after
/// another went through `pieces` and `fetch_ahead`'s locks and allocations each, review C-3).
#[derive(Clone, Copy)]
struct Last {
    end: u64,
    ahead: (u64, u64),
}
static LAST_END: Mutex<Option<HashMap<Ino, Last>>> = Mutex::new(None);
/// (tests: reads that queued their window)
#[cfg(test)]
static QUEUED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

thread_local! {
    /// The block this thread read last: a read inside it again (an engine reading an index in
    /// small reads) copies from it without the cache's locks. Blocks are named by their
    /// content, so it stays right whatever the cache drops meanwhile.
    static LAST_BLOCK: std::cell::RefCell<Option<(Key, Arc<Vec<u8>>)>> = const { std::cell::RefCell::new(None) };
}

/// The thread's last block when it is `k` (`len` bytes)
fn last_block(k: &Key, len: usize) -> Option<Arc<Vec<u8>>> {
    LAST_BLOCK.with(|c| c.borrow().as_ref().filter(|(x, d)| x == k && d.len() == len).map(|x| x.1.clone()))
}

/// Where the block holding byte `at` of a file laid out as `l` ends.
fn block_end(l: &Layout, at: u64) -> u64 {
    let (cstart, clen) = l.span(l.index(at));
    (cstart + ((at - cstart) / block() + 1) * block()).min(cstart + clen)
}

/// What a read of `ino` at `off` finds under the FS lock: the bytes of a file in memory
/// (copied into `buf`: Err(count)), or a store file's layout and chunks.
fn lookup(fs: &Fs, ino: Ino, buf: &mut [u8], off: u64) -> Result<Result<(Layout, Arc<Vec<Option<Hash>>>), usize>, c_int> {
    match fs.node(ino).map(|n| &n.data) {
        Some(Data::File(Content::Upper(v))) => {
            // (an offset past what a usize holds is past the end: not its low 32 bits)
            let start = usize::try_from(off).map_or(v.len(), |o| o.min(v.len()));
            let n = buf.len().min(v.len() - start);
            buf[..n].copy_from_slice(&v[start..start + n]);
            Ok(Err(n))
        }
        Some(Data::File(Content::Lower { size, chunks, starts })) => Ok(Ok((Layout { size: *size, starts: starts.clone() }, chunks.clone()))),
        Some(Data::Dir(_)) => Err(libc::EISDIR),
        _ => Err(libc::EBADF),
    }
}

/// Reads file content at `off` (the lock is not held while chunks are fetched).
fn read_at(ino: Ino, buf: &mut [u8], off: u64) -> Result<usize, c_int> {
    let lower = with(|fs| lookup(fs, ino, buf, off))?;
    read_lower(ino, buf, off, lower)
}

/// read_at past the FS lock (`lower`: what `lookup` found).
fn read_lower(ino: Ino, buf: &mut [u8], off: u64, lower: Result<(Layout, Arc<Vec<Option<Hash>>>), usize>) -> Result<usize, c_int> {
    let (layout, chunks) = match lower {
        Err(n) => return Ok(n),
        Ok(l) => l,
    };
    let size = layout.size;
    // (a read of nothing: else end - 1 below wraps, and every chunk of the file is fetched ahead)
    if off >= size || buf.is_empty() {
        return Ok(0);
    }
    let end = (off + buf.len() as u64).min(size);
    // the read's other pieces and the blocks after it: fetched at once by the fetcher threads
    // while this one fetches the first (unless reads before it queued them already)
    let (ahead_end, first_read, queued) = {
        let mut g = LAST_END.lock().unwrap_or_else(|e| e.into_inner());
        let m = g.get_or_insert_with(HashMap::default);
        if m.len() > 256 {
            m.clear();
        }
        let last = m.get(&ino).copied();
        let sequential = last.is_some_and(|l| l.end == off);
        let ahead_end = (end + block() * if sequential { SEQ_AHEAD } else { NEXT_AHEAD }).min(size);
        let queued = last.is_some_and(|l| off >= l.ahead.0 && ahead_end <= l.ahead.1);
        let ahead = if queued { last.unwrap().ahead } else { (off, block_end(&layout, ahead_end - 1)) };
        m.insert(ino, Last { end, ahead });
        (ahead_end, last.is_none(), queued)
    };
    // (what the game reads soon, when this read waits for the network: the page fetches it
    // onto the device meanwhile)
    let i = layout.index(off);
    if let Some(h) = chunks.get(i).copied().flatten() {
        let k = (h, ((off - layout.starts[i]) / block()) as u32);
        if last_block(&k, block_len(layout.span(i).1, k.1)).is_none() && !cached(&k) {
            hint(ino, &layout, &chunks, i, first_read);
        }
    }
    if !queued {
        #[cfg(test)]
        QUEUED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for p in pieces(off, end, ahead_end, &layout, &chunks, true) {
            fetch_ahead(p);
        }
    }
    let b = block();
    let mut at = off;
    let t = std::time::Instant::now();
    while at < end {
        let i = layout.index(at);
        let (cstart, clen) = layout.span(i);
        let from = at - cstart;
        let n = (end - at).min(clen - from);
        let dst = &mut buf[(at - off) as usize..(at - off + n) as usize];
        match chunks.get(i).copied().flatten() {
            None => dst.fill(0),
            Some(h) => {
                let (b0, b1) = ((from / b) as u32, ((from + n - 1) / b) as u32);
                if b0 == b1 {
                    if let Some(blk) = last_block(&(h, b0), block_len(clen, b0)) {
                        let s = (from - b0 as u64 * b) as usize;
                        dst.copy_from_slice(&blk[s..s + n as usize]);
                        at += n;
                        continue;
                    }
                }
                let blocks = chunk_blocks(&h, clen, b0, b1)?;
                if let Some(last) = blocks.last() {
                    LAST_BLOCK.with(|c| *c.borrow_mut() = Some(((h, b1), last.clone())));
                }
                let mut d = 0usize;
                for (k, blk) in blocks.iter().enumerate() {
                    let bstart = (b0 as u64 + k as u64) * b;
                    let s = (from + d as u64 - bstart) as usize;
                    let m = (blk.len() - s).min(n as usize - d);
                    dst[d..d + m].copy_from_slice(&blk[s..s + m]);
                    d += m;
                }
            }
        }
        at += n;
    }
    // (the run's trace: each chunk's first read, and how long this read waited)
    if !PREDICTING.with(|p| p.get()) {
        trace::read(ino, off, end, &layout, &chunks, t.elapsed());
    }
    // (a script: what it names, fetched ahead)
    predict_read(ino, off, end - off);
    Ok((end - off) as usize)
}

/// Files this big and more, copied from the store into memory to be written, are noted
/// (stderr): a game that writes into a big archive holds all of it.
const COPY_UP_NOTE: u64 = 16 << 20;

/// Turns a file from the store into one in memory, before it is written: its first `keep`
/// bytes (the rest is cut, as set_size does: truncating is not a copy of the whole).
fn copy_up(ino: Ino, keep: u64) -> Result<(), c_int> {
    let size = with(|fs| match fs.node(ino).map(|n| &n.data) {
        Some(Data::File(Content::Lower { size, .. })) => Some(*size),
        _ => None,
    });
    let Some(size) = size else { return Ok(()) };
    let size = size.min(keep);
    if size >= COPY_UP_NOTE {
        eprintln!("kura: a write copies {} MiB of a file from the store into memory (inode {ino})", size >> 20);
    }
    let mut v = Vec::new();
    grow(&mut v, upper_end(size, 0, UPPER_MAX)?)?;
    read_at(ino, &mut v, 0)?;
    with(|fs| {
        if let Some(Data::File(c @ Content::Lower { .. })) = fs.data_mut(ino) {
            *c = Content::Upper(v);
        }
    });
    Ok(())
}

/// The most bytes a file in memory holds: what a Vec can (isize::MAX: 2 GiB less one on wasm32).
const UPPER_MAX: usize = isize::MAX as usize;

/// Where `len` bytes written at `off` of a file in memory end, EFBIG past `max` (the file
/// offset is 64 bits and a usize 32 on wasm32: `off as usize` wrote at the offset's low half,
/// and the sum wrapped into a slice range that panicked).
fn upper_end(off: u64, len: usize, max: usize) -> Result<usize, c_int> {
    off.checked_add(len as u64).filter(|&e| e <= max as u64).map(|e| e as usize).ok_or(libc::EFBIG)
}

/// Makes `v` at least `len` bytes, the new ones zero: ENOSPC when the memory is not there (a
/// SetEndOfFile far out stopped the whole program in the allocator; Windows says the disk is full).
fn grow(v: &mut Vec<u8>, len: usize) -> Result<(), c_int> {
    if len > v.len() {
        v.try_reserve(len - v.len()).map_err(|_| libc::ENOSPC)?;
        v.resize(len, 0);
    }
    Ok(())
}

fn write_at(ino: Ino, data: &[u8], off: u64) -> Result<usize, c_int> {
    copy_up(ino, u64::MAX)?;
    with(|fs| fs.write_upper(ino, data, off))
}

fn set_size(ino: Ino, size: u64) -> Result<(), c_int> {
    copy_up(ino, size)?;
    with(|fs| fs.set_upper_size(ino, size))
}

impl Fs {
    /// write_at to a file in memory
    fn write_upper(&mut self, ino: Ino, data: &[u8], off: u64) -> Result<usize, c_int> {
        let Some(Data::File(Content::Upper(v))) = self.upper_mut(ino) else { return Err(libc::EBADF) };
        let end = upper_end(off, data.len(), UPPER_MAX)?;
        let old = v.len();
        grow(v, end)?;
        v[end - data.len()..end].copy_from_slice(data);
        // (the bytes the other processes need: these, and the zeros a write past the end made)
        self.dirt.bytes(ino, old.min(end - data.len()), end);
        if let Some(n) = self.meta_mut(ino) {
            n.mtime = now();
        }
        Ok(data.len())
    }
    /// set_size of a file in memory
    fn set_upper_size(&mut self, ino: Ino, size: u64) -> Result<(), c_int> {
        match self.upper_mut(ino) {
            Some(Data::File(Content::Upper(v))) => {
                let size = upper_end(size, 0, UPPER_MAX)?;
                let old = v.len();
                if size > v.len() {
                    grow(v, size)?;
                } else {
                    v.truncate(size);
                }
                // (cut then grown again before the others look: the zeros between are new)
                self.dirt.bytes(ino, old.min(size), old.max(size));
                Ok(())
            }
            Some(Data::Dir(_)) => Err(libc::EISDIR),
            _ => Err(libc::EINVAL),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// for crate::fs: absolute paths, results instead of errno

/// A path in a tree here, or outside them all (the host's file system: node only).
pub(crate) enum At<T> {
    Ours(Result<T, c_int>),
    Outside(String),
}

/// What [`crate::fs::Metadata`] tells.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Info {
    pub dir: bool,
    pub len: u64,
    pub mtime: i64,
    pub mode: u32,
    pub ino: u32,
}

impl Fs {
    fn info(&self, ino: Ino) -> Info {
        let n = self.node(ino).unwrap();
        Info { dir: matches!(n.data, Data::Dir(_)), len: self.file_size(ino), mtime: n.mtime, mode: n.mode, ino }
    }
}

/// `p` without ".", ".." or "//" (relative: from "/").
pub(crate) fn clean(p: &str) -> String {
    if trace() {
        eprintln!("vfs: {p}");
    }
    let mut parts: Vec<&str> = Vec::new();
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    let mut out = String::with_capacity(p.len());
    for c in parts {
        out.push('/');
        out.push_str(c);
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

/// What `p` resolves to, as `f` makes of it (a missing name: ENOENT).
fn at<T>(p: &str, follow: bool, f: impl FnOnce(&mut Fs, Ino) -> Result<T, c_int>) -> At<T> {
    with(|fs| match fs.resolve(p, follow) {
        Err(e) => At::Ours(Err(e)),
        Ok(Found::Outside(p)) => At::Outside(p),
        Ok(Found::Missing { .. }) => At::Ours(Err(libc::ENOENT)),
        Ok(Found::Node { ino, .. }) => At::Ours(f(fs, ino)),
    })
}

pub(crate) fn info_at(p: &str, follow: bool) -> At<Info> {
    at(p, follow, |fs, ino| Ok(fs.info(ino)))
}

/// The names in directory `p` and what they are.
pub(crate) fn list_at(p: &str) -> At<Vec<(String, Info)>> {
    at(p, true, |fs, ino| match &fs.node(ino).unwrap().data {
        Data::Dir(ch) => Ok(ch.iter().map(|(n, &i)| (n.clone(), fs.info(i))).collect()),
        _ => Err(libc::ENOTDIR),
    })
}

/// The real name of `name` in directory `p` when only its case differs (FindFirstFile and
/// CreateFile match without case).
pub(crate) fn name_nocase(p: &str, name: &str) -> At<Option<String>> {
    let lc = name.to_lowercase();
    at(p, true, |fs, ino| match &fs.node(ino).unwrap().data {
        Data::Dir(ch) => Ok(ch.keys().find(|k| same_nocase(k, name, &lc)).cloned()),
        _ => Err(libc::ENOTDIR),
    })
}

/// `k.to_lowercase() == lc` (`lc`: `name` lowercased) without a String per key: a lookup
/// lowercased every name of the directory under the FS lock (review C-3). Char by char is the
/// same but for Σ, whose lowercase depends on where it is in the word.
fn same_nocase(k: &str, name: &str, lc: &str) -> bool {
    if k.is_ascii() && name.is_ascii() {
        return k.eq_ignore_ascii_case(name);
    }
    if k.contains('Σ') || name.contains('Σ') {
        return k.to_lowercase() == lc;
    }
    k.chars().flat_map(char::to_lowercase).eq(lc.chars())
}

/// Opens `p` as a handle (released with [`release`]). Resolving, creating, checking the kind and
/// taking the handle are one critical section: two threads creating one missing name both made
/// a node (the second replaced the first in the folder, which then stayed for ever with its
/// writes nowhere), and a file unlinked between the look-up and the handle left a handle on a
/// freed, maybe reused, inode.
pub(crate) fn open_handle(p: &str, flags: c_int, mode: u32) -> At<u32> {
    let follow = flags & libc::O_NOFOLLOW == 0;
    let opened = with(|fs| {
        let (ino, path) = match fs.resolve(p, follow) {
            Err(e) => return At::Ours(Err(e)),
            Ok(Found::Outside(p)) => return At::Outside(p),
            Ok(Found::Node { ino, path }) => {
                if flags & libc::O_CREAT != 0 && flags & libc::O_EXCL != 0 {
                    return At::Ours(Err(libc::EEXIST));
                }
                (ino, Some(path))
            }
            Ok(Found::Missing { parent, name, .. }) => {
                if flags & libc::O_CREAT == 0 {
                    return At::Ours(Err(libc::ENOENT));
                }
                let ino = fs.add(Data::File(Content::Upper(Vec::new())), mode & 0o7777, now());
                if let Some(Data::Dir(ch)) = fs.data_mut(parent) {
                    ch.insert(name, ino);
                }
                (ino, None)
            }
        };
        let kind = fs.node(ino).map(|n| match n.data {
            Data::Dir(_) => 0,
            Data::File(_) => 1,
            Data::Link(_) => 2,
        });
        match kind {
            Some(0) if flags & libc::O_ACCMODE != libc::O_RDONLY => return At::Ours(Err(libc::EISDIR)),
            Some(2) => return At::Ours(Err(libc::ELOOP)),
            None => return At::Ours(Err(libc::ENOENT)),
            _ => {}
        }
        At::Ours(Ok((fs.new_handle(ino, flags), ino, kind == Some(1), path)))
    });
    let (h, ino, file, path) = match opened {
        At::Ours(Ok(o)) => o,
        At::Ours(Err(e)) => return At::Ours(Err(e)),
        At::Outside(p) => return At::Outside(p),
    };
    // (truncating may copy from the store: outside the lock, the handle holds the node)
    if file && flags & libc::O_TRUNC != 0 && flags & libc::O_ACCMODE != libc::O_RDONLY {
        if let Err(e) = set_size(ino, 0) {
            release(h);
            return At::Ours(Err(e));
        }
    }
    if let (true, Some(path)) = (file, &path) {
        note_name(ino, path);
    }
    At::Ours(Ok(h))
}

/// Closes a handle.
pub(crate) fn release(h: u32) {
    with(|fs| fs.close_handle(h))
}
impl Fs {
    fn close_handle(&mut self, h: u32) {
        if let Some(handle) = self.handles.remove(&h) {
            if let Some(Some(n)) = self.nodes.get_mut(handle.ino as usize) {
                n.open_refs = n.open_refs.saturating_sub(1);
                self.dirt.refs(handle.ino);
            }
            self.gc(handle.ino);
        }
    }
}

pub(crate) fn mkdir_at(p: &str) -> At<()> {
    with(|fs| match fs.resolve(p, false) {
        Err(e) => At::Ours(Err(e)),
        Ok(Found::Outside(p)) => At::Outside(p),
        Ok(Found::Node { .. }) => At::Ours(Err(libc::EEXIST)),
        Ok(Found::Missing { parent, name, .. }) => {
            let ino = fs.add(Data::Dir(BTreeMap::new()), 0o755, now());
            if let Some(Data::Dir(ch)) = fs.data_mut(parent) {
                ch.insert(name, ino);
            }
            if let Some(n) = fs.node_mut(parent) {
                n.nlink += 1;
            }
            At::Ours(Ok(()))
        }
    })
}

/// unlink (dir: false) or rmdir (dir: true)
pub(crate) fn remove_at(p: &str, dir: bool) -> At<()> {
    let Some((parent_path, name)) = p.rsplit_once('/') else { return At::Ours(Err(libc::EINVAL)) };
    let parent_path = if parent_path.is_empty() { "/" } else { parent_path };
    with(|fs| {
        let parent = match fs.resolve(parent_path, true) {
            Err(e) => return At::Ours(Err(e)),
            Ok(Found::Outside(_)) => return At::Outside(p.to_string()),
            Ok(Found::Missing { .. }) => return At::Ours(Err(libc::ENOENT)),
            Ok(Found::Node { ino, .. }) => ino,
        };
        let Some(Data::Dir(ch)) = fs.node(parent).map(|n| &n.data) else { return At::Ours(Err(libc::ENOTDIR)) };
        let Some(&ino) = ch.get(name) else { return At::Ours(Err(libc::ENOENT)) };
        match (&fs.node(ino).unwrap().data, dir) {
            (Data::Dir(c), true) => {
                if !c.is_empty() {
                    return At::Ours(Err(libc::ENOTEMPTY));
                }
                if let Some(n) = fs.node_mut(ino) {
                    n.nlink = 0;
                }
                if let Some(n) = fs.node_mut(parent) {
                    n.nlink = n.nlink.saturating_sub(1);
                }
            }
            (Data::Dir(_), false) => return At::Ours(Err(libc::EISDIR)),
            (_, true) => return At::Ours(Err(libc::ENOTDIR)),
            _ => {}
        }
        fs.unlink_name(parent, name);
        At::Ours(Ok(()))
    })
}

/// Outside: both paths are (one inside and one outside: EXDEV).
pub(crate) fn rename_at(a: &str, b: &str) -> At<()> {
    with(|fs| {
        let src = fs.resolve(a, false);
        let dst = fs.resolve(b, false);
        let (src_ino, sp, sn) = match src {
            Err(e) => return At::Ours(Err(e)),
            Ok(Found::Outside(p)) => {
                return match dst {
                    Ok(Found::Outside(_)) => At::Outside(p),
                    _ => At::Ours(Err(libc::EXDEV)),
                }
            }
            Ok(Found::Missing { .. }) => return At::Ours(Err(libc::ENOENT)),
            Ok(Found::Node { ino, path }) => {
                let (d, n) = path.rsplit_once('/').unwrap();
                let parent = match fs.resolve(if d.is_empty() { "/" } else { d }, true) {
                    Ok(Found::Node { ino, .. }) => ino,
                    _ => return At::Ours(Err(libc::EXDEV)), // a mount point itself
                };
                (ino, parent, n.to_string())
            }
        };
        let src_dir = matches!(fs.node(src_ino).map(|n| &n.data), Some(Data::Dir(_)));
        // (a mount's root stays where it is mounted, as a mount point does: EBUSY)
        if fs.mounts.iter().any(|m| m.1 == src_ino) {
            return At::Ours(Err(libc::EBUSY));
        }
        let (dp, dn, replaced) = match dst {
            Err(e) => return At::Ours(Err(e)),
            Ok(Found::Outside(_)) => return At::Ours(Err(libc::EXDEV)),
            Ok(Found::Missing { parent, name, .. }) => (parent, name, None),
            Ok(Found::Node { ino, path }) => {
                if ino == src_ino {
                    return At::Ours(Ok(()));
                }
                match (fs.node(ino).map(|n| &n.data), src_dir) {
                    (Some(Data::Dir(c)), true) if !c.is_empty() => return At::Ours(Err(libc::ENOTEMPTY)),
                    (Some(Data::Dir(_)), false) => return At::Ours(Err(libc::EISDIR)),
                    (Some(Data::Dir(_)), true) => {}
                    (_, true) => return At::Ours(Err(libc::ENOTDIR)),
                    _ => {}
                }
                if fs.mounts.iter().any(|m| m.1 == ino) {
                    return At::Ours(Err(libc::EBUSY));
                }
                let (d, n) = path.rsplit_once('/').unwrap();
                let Ok(Found::Node { ino: parent, .. }) = fs.resolve(if d.is_empty() { "/" } else { d }, true) else {
                    return At::Ours(Err(libc::EXDEV));
                };
                (parent, n.to_string(), Some(ino))
            }
        };
        // (a directory cannot go inside itself: its subtree would hang from no name)
        if src_dir && fs.within(dp, src_ino) {
            return At::Ours(Err(libc::EINVAL));
        }
        if let Some(old) = replaced {
            // (an empty directory replaced goes as rmdir takes it: its own links and its
            // parent's ".." link)
            if src_dir {
                if let Some(n) = fs.node_mut(old) {
                    n.nlink = 0;
                }
                if let Some(n) = fs.node_mut(dp) {
                    n.nlink = n.nlink.saturating_sub(1);
                }
            }
            fs.unlink_name(dp, &dn);
        }
        if let Some(Data::Dir(ch)) = fs.data_mut(sp) {
            ch.remove(&sn);
        }
        if let Some(Data::Dir(ch)) = fs.data_mut(dp) {
            ch.insert(dn, src_ino);
        }
        // (a directory's ".." moves to its new parent)
        if src_dir && sp != dp {
            if let Some(n) = fs.node_mut(sp) {
                n.nlink = n.nlink.saturating_sub(1);
            }
            if let Some(n) = fs.node_mut(dp) {
                n.nlink += 1;
            }
        }
        At::Ours(Ok(()))
    })
}

// handles

pub(crate) fn h_info(h: u32) -> Option<Info> {
    with(|fs| fs.handles.get(&h).map(|x| x.ino).map(|i| fs.info(i)))
}

fn handle(h: u32) -> Option<(Ino, u64, c_int)> {
    with(|fs| fs.handles.get(&h).map(|x| (x.ino, x.pos, x.flags)))
}

fn set_pos(h: u32, pos: u64) {
    with(|fs| {
        if let Some(x) = fs.handles.get_mut(&h) {
            x.pos = pos;
        }
    })
}

pub(crate) fn h_read(h: u32, buf: &mut [u8]) -> Result<usize, c_int> {
    // (the handle and its file's content in one lock)
    let (ino, pos, lower) = with(|fs| {
        let x = fs.handles.get(&h).ok_or(libc::EBADF)?;
        let (ino, pos) = (x.ino, x.pos);
        if x.flags & libc::O_ACCMODE == libc::O_WRONLY {
            return Err(libc::EBADF);
        }
        Ok((ino, pos, lookup(fs, ino, buf, pos)?))
    })?;
    if trace() {
        eprintln!("vfs: read {h} ino {ino} {}@{pos}", buf.len());
    }
    let n = read_lower(ino, buf, pos, lower)?;
    set_pos(h, pos + n as u64);
    Ok(n)
}

/// Has the store's blocks of bytes `off..off + len` of handle `h`'s file fetched by the fetcher
/// threads, without waiting for them (a movie's next seconds: kura::video).
pub(crate) fn h_prefetch(h: u32, off: u64, len: u64) {
    let Some((ino, _, _)) = handle(h) else { return };
    let want = with(|fs| match fs.node(ino).map(|n| &n.data) {
        Some(Data::File(Content::Lower { size, chunks, starts })) => {
            let l = Layout { size: *size, starts: starts.clone() };
            let end = off.saturating_add(len).min(l.size);
            pieces(off, end, end, &l, chunks, false)
        }
        _ => Vec::new(),
    });
    for p in want {
        fetch_ahead(p);
    }
}

pub(crate) fn h_write(h: u32, data: &[u8]) -> Result<usize, c_int> {
    let (ino, pos, flags) = handle(h).ok_or(libc::EBADF)?;
    if flags & libc::O_ACCMODE == libc::O_RDONLY {
        return Err(libc::EBADF);
    }
    let at = if flags & libc::O_APPEND != 0 { with(|fs| fs.file_size(ino)) } else { pos };
    let n = write_at(ino, data, at)?;
    set_pos(h, at + n as u64);
    Ok(n)
}

pub(crate) fn h_seek(h: u32, off: i64, whence: c_int) -> Result<u64, c_int> {
    with(|fs| {
        let x = fs.handles.get(&h).ok_or(libc::EBADF)?;
        let base = match whence {
            libc::SEEK_SET => 0,
            libc::SEEK_CUR => x.pos as i64,
            libc::SEEK_END => fs.file_size(x.ino) as i64,
            _ => return Err(libc::EINVAL),
        };
        let new = base + off;
        if new < 0 {
            return Err(libc::EINVAL);
        }
        fs.handles.get_mut(&h).unwrap().pos = new as u64;
        Ok(new as u64)
    })
}

pub(crate) fn h_set_len(h: u32, size: u64) -> Result<(), c_int> {
    let (ino, _, _) = handle(h).ok_or(libc::EBADF)?;
    set_size(ino, size)
}

/// The page: frees what `vnhost_vfs_export` returned.
#[no_mangle]
pub unsafe extern "C" fn vnhost_free(p: *mut c_void) {
    libc::free(p)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Random puts, replacements (other lengths) and padded gets against a model: a list of
    /// (hash, length) oldest first. After each step `bytes` is the sum of what is held, the
    /// order holds each hash once, the cache is within its limit (or holds one chunk bigger
    /// than it at most: none), and what is held is the newest suffix of the model.
    #[test]
    fn cache_accounts_for_what_it_holds() {
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        /// The policy again, written plainly: two lists, oldest (least recent) first.
        #[derive(Default)]
        struct Model {
            prob: Vec<(Hash, usize, u32)>,
            prot: Vec<(Hash, usize)>,
        }
        impl Model {
            fn bytes(&self) -> usize {
                self.prob.iter().map(|e| e.1).sum::<usize>() + self.prot.iter().map(|e| e.1).sum::<usize>()
            }
            fn read(&mut self, h: Hash) {
                if let Some(i) = self.prot.iter().position(|e| e.0 == h) {
                    let e = self.prot.remove(i);
                    self.prot.push(e);
                } else if let Some(i) = self.prob.iter().position(|e| e.0 == h) {
                    self.prob[i].2 += 1;
                    if self.prob[i].2 >= 2 {
                        let e = self.prob.remove(i);
                        self.prot.push((e.0, e.1));
                    }
                }
            }
            fn set_len(&mut self, h: Hash, len: usize) -> bool {
                if let Some(e) = self.prob.iter_mut().find(|e| e.0 == h) {
                    e.1 = len;
                } else if let Some(e) = self.prot.iter_mut().find(|e| e.0 == h) {
                    e.1 = len;
                } else {
                    return false;
                }
                true
            }
            fn trim(&mut self, limit: usize, keep: Hash) {
                while self.prot.iter().map(|e| e.1).sum::<usize>() > limit / PROTECTED.1 * PROTECTED.0 {
                    let e = self.prot.remove(0);
                    self.prob.push((e.0, e.1, 1));
                }
                while self.bytes() > limit {
                    if let Some(i) = self.prob.iter().position(|e| e.0 != keep) {
                        self.prob.remove(i);
                    } else if let Some(i) = self.prot.iter().position(|e| e.0 != keep) {
                        self.prot.remove(i);
                    } else {
                        self.prob.retain(|e| e.0 != keep);
                        self.prot.retain(|e| e.0 != keep);
                    }
                }
            }
        }
        for _ in 0..300 {
            let limit = 1 + rnd(4000) as usize;
            let keys = 1 + rnd(12);
            let mut c = Cache::default();
            let mut m = Model::default();
            for _ in 0..200 {
                let mut h = [0u8; 16];
                h[0] = rnd(keys) as u8;
                let len = rnd(1200) as usize;
                if rnd(3) == 0 {
                    let got = c.get(&h, len, limit);
                    let had = m.set_len(h, len);
                    assert_eq!(got.is_some(), had);
                    if let Some(d) = got {
                        assert_eq!(d.len(), len);
                        m.read(h);
                        m.trim(limit, h);
                    }
                } else {
                    let read = rnd(2) == 0;
                    c.put(&h, Arc::new(vec![h[0]; len]), read, limit);
                    if !m.set_len(h, len) {
                        m.prob.push((h, len, read as u32));
                    } else if read {
                        m.read(h);
                    }
                    m.trim(limit, h);
                }
                let sum: usize = c.map.values().map(|e| e.data.len()).sum();
                let psum: usize = c.map.values().filter(|e| e.protected).map(|e| e.data.len()).sum();
                assert_eq!((c.bytes, c.protected_bytes), (sum, psum));
                assert!(c.bytes <= limit);
                assert_eq!(c.probation.len() + c.protected.len(), c.map.len());
                assert_eq!(c.probation.values().copied().collect::<Vec<_>>(), m.prob.iter().map(|e| e.0).collect::<Vec<_>>());
                assert_eq!(c.protected.values().copied().collect::<Vec<_>>(), m.prot.iter().map(|e| e.0).collect::<Vec<_>>());
                for e in m.prob.iter().map(|e| (e.0, e.1)).chain(m.prot.iter().copied()) {
                    assert_eq!(c.map[&e.0].data.len(), e.1);
                }
            }
        }
    }

    /// What the two segments are for: chunks read again and again (scripts, the UI) stay while
    /// a voice or a movie is read through once, and while chunks are fetched ahead of it.
    #[test]
    fn cache_keeps_what_is_read_again_past_a_long_read() {
        let limit = 64 * 1000;
        let mut c = Cache::default();
        let h = |i: u32| {
            let mut h = [0u8; 16];
            h[..4].copy_from_slice(&i.to_le_bytes());
            h
        };
        // 20 chunks of 1000 read twice: protected
        for i in 0..20 {
            c.put(&h(i), Arc::new(vec![0; 1000]), true, limit);
            assert!(c.get(&h(i), 1000, limit).is_some());
        }
        // a movie: 500 chunks fetched ahead, each read once
        for i in 1000..1500 {
            c.put(&h(i), Arc::new(vec![0; 1000]), false, limit);
            assert!(c.get(&h(i), 1000, limit).is_some());
        }
        for i in 0..20 {
            assert!(c.contains(&h(i)), "chunk {i} read twice went");
        }
        assert!(c.bytes <= limit);
        // read again and again but more than the protected share: the least recent go
        for r in 0..3 {
            for i in 2000..2060 {
                if r == 0 {
                    c.put(&h(i), Arc::new(vec![0; 1000]), true, limit);
                } else {
                    let _ = c.get(&h(i), 1000, limit);
                }
            }
        }
        assert!(c.protected_bytes <= limit / 4 * 3);
        assert!(c.bytes <= limit);
        // the movie's first chunk went: read again, it counts
        assert!(!c.contains(&h(1000)));
        let before = c.refetched;
        c.put(&h(1000), Arc::new(vec![0; 1000]), true, limit);
        assert_eq!(c.refetched, before + 1);
        c.put(&h(1001), Arc::new(vec![0; 1000]), false, limit);
        assert_eq!(c.refetched, before + 1, "fetched ahead: not a read");
    }

    /// The tests that mount files in the one file system (FS) and store, one at a time.
    pub(super) static TEST_FS: Mutex<()> = Mutex::new(());

    /// For tests of other modules that open the host's files (video's temporary .vnv): none of
    /// these tests' mounts meanwhile (most mount "/", and leave it mounted), held while the guard
    /// lives. (They failed when run with these: their file was looked for in a test's tree.)
    pub(crate) fn host_files() -> std::sync::MutexGuard<'static, ()> {
        let g = TEST_FS.lock().unwrap_or_else(|e| e.into_inner());
        *FS.lock().unwrap_or_else(|e| e.into_inner()) = None;
        g
    }

    /// The store, for these tests: chunks by name, and the names fetched.
    static STORE: Mutex<Option<(HashMap<String, Vec<u8>>, Vec<String>)>> = Mutex::new(None);

    /// The hints these tests' reads sent (host/vnfs.js vnvm_fetch_hint's names).
    static HINTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    #[no_mangle]
    extern "C" fn vnvm_fetch_hint(names: *const c_char, len: size_t) {
        let s = unsafe { std::slice::from_raw_parts(names as *const u8, len) };
        HINTS.lock().unwrap().extend(String::from_utf8_lossy(s).split('\n').map(String::from));
    }

    #[no_mangle]
    extern "C" fn vnvm_fetch(name: *const c_char, len: *mut size_t) -> *mut u8 {
        let name = unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy().into_owned();
        let mut g = STORE.lock().unwrap();
        let (m, log) = g.get_or_insert_with(Default::default);
        log.push(name.clone());
        // (a range: `<name>#<from>+<len>`, as host/vnfs.js; fewer bytes past a short chunk's end)
        let (key, range) = match name.split_once('#') {
            Some((k, r)) => {
                let (a, n) = r.split_once('+').unwrap();
                let n = n.split('/').next().unwrap();
                (k.to_string(), Some((a.parse::<usize>().unwrap(), n.parse::<usize>().unwrap())))
            }
            None => (name.clone(), None),
        };
        let Some(d) = m.get(&key) else { return std::ptr::null_mut() };
        let d = match range {
            Some((a, n)) => &d[a.min(d.len())..(a + n).min(d.len())],
            None => &d[..],
        };
        unsafe {
            let p = libc::malloc(d.len().max(1)) as *mut u8;
            std::ptr::copy_nonoverlapping(d.as_ptr(), p, d.len());
            *len = d.len();
            p
        }
    }

    /// Store files (chunks of 64 bytes, some all zero, the last short; every other file in
    /// chunks of 1 to 150 bytes, as cut by content), then random writes,
    /// truncations and reads against a model of each file's bytes. A truncation to 0 of a file
    /// still in the store fetches nothing; one to n only the chunks before n (and the next).
    #[test]
    fn store_files_written_and_cut() {
        let _g = TEST_FS.lock().unwrap_or_else(|e| e.into_inner());
        let mut seed = 0x510e527fade682d1u64;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut fs = Fs { next_handle: 1, ..Default::default() };
        let root = fs.add(Data::Dir(BTreeMap::new()), 0o755, 0);
        let mut tsv = String::from("vnvm-tree\t2\t64\n");
        let mut models = Vec::new();
        let mut chunks = HashMap::new();
        for f in 0..12 {
            let size = rnd(64 * 9) + 1;
            let data: Vec<u8> = (0..size).map(|i| if (i / 64) % 4 == 2 { 0 } else { 1 + rnd(255) as u8 }).collect();
            let mut names = Vec::new();
            // (odd files: chunks of any length, as content-defined ones)
            let mut spans = Vec::new();
            let mut at = 0usize;
            while at < data.len() {
                let n = if f % 2 == 1 { 1 + rnd(150) as usize } else { 64 };
                let n = n.min(data.len() - at);
                spans.push(&data[at..at + n]);
                at += n;
            }
            for c in spans {
                let len = format!(":{}", c.len());
                if c.iter().all(|&b| b == 0) {
                    names.push(len);
                    continue;
                }
                let h: String = (0..16).map(|_| format!("{:02x}", rnd(256))).collect();
                // (stored chunks may end short: their zero tail is cut)
                let mut stored = c.to_vec();
                while stored.last() == Some(&0) {
                    stored.pop();
                }
                chunks.insert(format!("chunks/{}/{h}", &h[..2]), stored);
                names.push(format!("{h}{len}"));
            }
            tsv += &format!("f\t644\t0\tf{f}\t{size}\t{}\n", names.join(","));
            models.push(data);
        }
        fs.load_tree(root, &tsv);
        let inos: Vec<Ino> = (0..12).map(|f| match fs.node(root).map(|n| &n.data) {
            Some(Data::Dir(c)) => c[&format!("f{f}")],
            _ => unreachable!(),
        }).collect();
        *FS.lock().unwrap() = Some(fs);
        *STORE.lock().unwrap() = Some((chunks, Vec::new()));
        // (what was fetched since, once the fetcher threads are done: a chunk fetched ahead
        // for one file must not land in the next one's count)
        let fetched = || {
            let mut g = AHEAD.lock().unwrap_or_else(|e| e.into_inner());
            while g.as_ref().is_some_and(|a| !a.busy.is_empty()) {
                g = AHEAD_DONE.wait(g).unwrap_or_else(|e| e.into_inner());
            }
            drop(g);
            std::mem::take(&mut STORE.lock().unwrap().as_mut().unwrap().1)
        };
        // the first operation on each file: a truncation, checked for what it fetched
        for (f, &ino) in inos.iter().enumerate() {
            let n = if f % 3 == 0 { 0 } else { rnd(models[f].len() as u64 + 100) };
            fetched();
            set_size(ino, n).unwrap();
            let got: std::collections::HashSet<String> = fetched().into_iter().map(|n| n.split('#').next().unwrap().to_string()).collect();
            // (and the chunk after them, fetched ahead as after any read; odd files' chunks
            // are 1 byte or more)
            let unit = if f % 2 == 1 { 1 } else { 64 };
            let need = (n.min(models[f].len() as u64)).div_ceil(unit) as usize;
            let need = if need == 0 { 0 } else { need + 1 };
            assert!(got.len() <= need, "f{f} cut to {n}: fetched {} chunks, needs {need}", got.len());
            models[f].resize(n as usize, 0);
        }
        for _ in 0..2000 {
            let f = rnd(12) as usize;
            let (ino, m) = (inos[f], &mut models[f]);
            match rnd(3) {
                0 => {
                    let off = rnd(m.len() as u64 + 80);
                    let data: Vec<u8> = (0..rnd(150)).map(|_| rnd(256) as u8).collect();
                    assert_eq!(write_at(ino, &data, off), Ok(data.len()));
                    let end = off as usize + data.len();
                    if m.len() < end {
                        m.resize(end, 0);
                    }
                    m[off as usize..end].copy_from_slice(&data);
                }
                1 => {
                    let n = rnd(m.len() as u64 + 100);
                    set_size(ino, n).unwrap();
                    m.resize(n as usize, 0);
                }
                _ => {
                    let off = rnd(m.len() as u64 + 10);
                    let mut buf = vec![0xaa; rnd(200) as usize];
                    let n = read_at(ino, &mut buf, off).unwrap();
                    let want = &m[(off as usize).min(m.len())..(off as usize + buf.len()).min(m.len())];
                    assert_eq!(&buf[..n], want);
                }
            }
        }
    }

    /// `n` chunks `chunk` bytes long, as a layout.
    fn even(chunk: u64, n: usize) -> Layout {
        Layout { size: chunk * n as u64, starts: Arc::new((0..=n as u64).map(|i| i * chunk).collect()) }
    }

    /// A file's chunk lengths: the chunk of each byte, each chunk's span; a list whose lengths do
    /// not add up to the size, or a chunk with no length, is refused.
    #[test]
    fn chunk_layout() {
        let (h, s) = parse_chunks("00112233445566778899aabbccddeeff:3,:5,ffeeddccbbaa99887766554433221100:2", 10).unwrap();
        assert_eq!((h.len(), h[1]), (3, None));
        let l = Layout { size: 10, starts: s };
        assert_eq!((0..10).map(|at| l.index(at)).collect::<Vec<_>>(), [0, 0, 0, 1, 1, 1, 1, 1, 2, 2]);
        assert_eq!([l.span(0), l.span(1), l.span(2)], [(0, 3), (3, 5), (8, 2)]);
        assert!(parse_chunks("00112233445566778899aabbccddeeff:3", 4).is_none());
        assert!(parse_chunks("00112233445566778899aabbccddeeff:0", 0).is_none());
        assert!(parse_chunks("00112233445566778899aabbccddeeff", 9).is_none());
        assert_eq!(parse_chunks("", 0).unwrap().0.len(), 0);
    }

    /// The pieces fetched for a read (block() is 24 in tests, PIECE 4 blocks): every block of
    /// the read and up to its ahead end, by chunk, in pieces of at most PIECE; the read's first
    /// piece left to the reader; all-zero chunks none.
    #[test]
    fn pieces_cases() {
        let h = |i: u8| Some([i; 16]);
        let p = |c: u8, clen: u64, first: u32, n: u32| Piece { h: [c; 16], clen, first, n };
        // one chunk of 1000 bytes (42 blocks): a read of 0..100 (blocks 0..=4), ahead to 124
        // (block 5): the reader 0..=3, the fetchers 4 and 5
        let l = even(1000, 1);
        assert_eq!(pieces(0, 100, 124, &l, &[h(1)], true), [p(1, 1000, 4, 2)]);
        assert_eq!(pieces(0, 100, 124, &l, &[h(1)], false), [p(1, 1000, 0, 4), p(1, 1000, 4, 2)]);
        // a scattered read in a big chunk: only its blocks (20, 21: the reader's) and the next
        assert_eq!(pieces(500, 520, 544, &l, &[h(1)], true), [p(1, 1000, 22, 1)]);
        // over three chunks, the middle one all zero: the reader 3..=4 of the first, the third's
        // blocks to the fetchers
        let l = even(100, 3);
        assert_eq!(pieces(90, 210, 210, &l, &[h(1), None, h(3)], true), [p(3, 100, 0, 1)]);
        // a read starting in a zero chunk: no piece of its own
        assert_eq!(pieces(110, 230, 230, &l, &[h(1), None, h(3)], true), [p(3, 100, 0, 2)]);
        // the file's end
        assert_eq!(pieces(299, 300, 300, &l, &[h(1), h(2), h(3)], true), []);
        assert_eq!(pieces(300, 300, 300, &l, &[h(1), h(2), h(3)], false), []);
    }

    proptest::proptest! {
        /// Any layout (chunks of 1 to 200 bytes, some all zero), any read and ahead window: the
        /// pieces and the read's own first piece cover exactly the stored blocks of
        /// off..ahead_end, once each, in pieces of one chunk of at most PIECE blocks, within
        /// it; the reader's own piece holds the block of `off`.
        #[test]
        fn pieces_cover(lens in proptest::collection::vec(1u64..200, 1..12), zero in proptest::collection::vec(proptest::bool::ANY, 12), a in 0u64..3000, len in 1u64..600, more in 0u64..300) {
            let mut starts = vec![0u64];
            for n in &lens { starts.push(starts.last().unwrap() + n); }
            let size = *starts.last().unwrap();
            let l = Layout { size, starts: Arc::new(starts) };
            let chunks: Vec<Option<Hash>> = (0..lens.len()).map(|i| (!zero[i]).then_some([i as u8; 16])).collect();
            let off = a % size;
            let end = (off + len).min(size);
            let ahead_end = (end + more).min(size);
            let b = block();
            let want: std::collections::BTreeSet<(u8, u32)> = (off..ahead_end).filter_map(|x| {
                let i = l.index(x);
                chunks[i].map(|h| (h[0], ((x - l.starts[i]) / b) as u32))
            }).collect();
            let all = pieces(off, end, ahead_end, &l, &chunks, false);
            let own = pieces(off, end, ahead_end, &l, &chunks, true);
            let mut got = std::collections::BTreeSet::new();
            for p in &all {
                let i = p.h[0] as usize;
                proptest::prop_assert!(p.n >= 1 && p.n <= PIECE);
                proptest::prop_assert_eq!(p.clen, lens[i]);
                proptest::prop_assert!((p.first + p.n - 1) as u64 * b < p.clen);
                for k in p.first..p.first + p.n {
                    proptest::prop_assert!(got.insert((p.h[0], k)), "block twice");
                }
            }
            proptest::prop_assert_eq!(&got, &want);
            // (the reader's: the same but its own first blocks, the read's in its first chunk
            // from `off`'s, PIECE at most)
            let i = l.index(off);
            let mut rest = std::collections::BTreeSet::new();
            for p in &own {
                for k in p.first..p.first + p.n {
                    proptest::prop_assert!(rest.insert((p.h[0], k)));
                }
            }
            let mine: std::collections::BTreeSet<(u8, u32)> = match chunks[i] {
                Some(h) => {
                    let b0 = ((off - l.starts[i]) / b) as u32;
                    let last = ((end.min(l.starts[i + 1]) - 1 - l.starts[i]) / b) as u32;
                    (b0..=last.min(b0 + PIECE - 1)).map(|k| (h[0], k)).collect()
                }
                None => Default::default(),
            };
            proptest::prop_assert!(rest.is_disjoint(&mine));
            proptest::prop_assert_eq!(rest.union(&mine).copied().collect::<std::collections::BTreeSet<_>>(), want);
        }
    }

    /// A scattered read of a big chunk fetches its own blocks and the next one, not the chunk
    /// (the reason for blocks: docs/chunk-size.md), and the bytes are the chunk's.
    #[test]
    fn scattered_read_fetches_blocks() {
        let _g = TEST_FS.lock().unwrap_or_else(|e| e.into_inner());
        let size = 24 * 1000u64;
        let data: Vec<u8> = (0..size).map(|i| (i * 7 + 3) as u8 | 1).collect();
        let h = "abababababababababababababababab";
        let mut fs = Fs { next_handle: 1, ..Default::default() };
        let root = fs.add(Data::Dir(BTreeMap::new()), 0o755, 0);
        fs.load_tree(root, &format!("vnvm-tree\t2\t64\nf\t644\t0\tbig\t{size}\t{h}:{size}\n"));
        let ino = match fs.node(root).map(|n| &n.data) {
            Some(Data::Dir(c)) => c["big"],
            _ => unreachable!(),
        };
        *FS.lock().unwrap() = Some(fs);
        let mut m = HashMap::new();
        m.insert(format!("chunks/ab/{h}"), data.clone());
        *STORE.lock().unwrap() = Some((m, Vec::new()));
        let mut seed = 77u64;
        for _ in 0..50 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let off = (seed >> 33) % (size - 100);
            let len = 1 + (seed >> 20) % 60;
            let mut buf = vec![0; len as usize];
            assert_eq!(read_at(ino, &mut buf, off), Ok(len as usize));
            assert_eq!(&buf[..], &data[off as usize..(off + len) as usize]);
            // (the fetchers done)
            let mut g = AHEAD.lock().unwrap_or_else(|e| e.into_inner());
            while g.as_ref().is_some_and(|a| !a.busy.is_empty()) {
                g = AHEAD_DONE.wait(g).unwrap_or_else(|e| e.into_inner());
            }
            drop(g);
            let log = std::mem::take(&mut STORE.lock().unwrap().as_mut().unwrap().1);
            let bytes: u64 = log.iter().map(|n| n.rsplit('+').next().unwrap().split('/').next().unwrap().parse::<u64>().unwrap()).sum();
            // (a fetcher thread's ends "!a": host/fetch-bridge.js serves the game's reads first)
            assert!(log.iter().all(|n| n.ends_with("/24") || n.ends_with("/24!a")), "{log:?}");
            // its blocks (len <= 60 spans 4 of 24 bytes at most) and NEXT_AHEAD more
            assert!(bytes <= (4 + NEXT_AHEAD) * 24, "read {off}+{len} fetched {bytes} bytes: {log:?}");
        }
    }

    /// Small reads one after another (an engine reading an index): each read gives the file's
    /// bytes, but only a read whose window passes the blocks queued before queues more (one per
    /// block, not one per read), and reads inside the thread's last block take nothing from the
    /// store; with the cache emptied between reads, and reads that jump about, the bytes stay right.
    #[test]
    fn small_reads_queue_once_per_block() {
        let _g = TEST_FS.lock().unwrap_or_else(|e| e.into_inner());
        let h = |i: u32| format!("{:032x}", 0x5ead_0000u128 + i as u128);
        let mut m = HashMap::new();
        let mut model = Vec::new();
        let mut list = Vec::new();
        for i in 0..20u32 {
            // (chunks of 100 bytes but one of 37 and one all zero; blocks are 24 bytes here)
            let len = if i == 7 { 37 } else { 100 };
            let c: Vec<u8> = (0..len).map(|k| if i == 11 { 0 } else { (i * 31 + k * 7 + 1) as u8 }).collect();
            if i == 11 {
                list.push(format!(":{len}"));
            } else {
                m.insert(format!("chunks/{}/{}", &h(i)[..2], h(i)), c.clone());
                list.push(format!("{}:{len}", h(i)));
            }
            model.extend(c);
        }
        let tsv = format!("vnvm-tree\t2\t64\nf\t644\t0\tindex.pck\t{}\t{}\n", model.len(), list.join(","));
        let mut fs = Fs { next_handle: 1, ..Default::default() };
        let root = fs.add(Data::Dir(BTreeMap::new()), 0o755, 0);
        // (not at "/": the other tests' files (video's temporary .vnv) stay outside it)
        fs.mounts.push(("/vfs-test".into(), root));
        fs.load_tree(root, &tsv);
        *FS.lock().unwrap() = Some(fs);
        *STORE.lock().unwrap() = Some((m, Vec::new()));
        *CACHE.lock().unwrap() = None;
        *LAST_END.lock().unwrap() = None;
        let At::Ours(Ok(fd)) = open_handle("/vfs-test/index.pck", libc::O_RDONLY, 0) else { panic!("open") };
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        // front to back in reads of 1 to 8 bytes
        let q0 = QUEUED.load(std::sync::atomic::Ordering::Relaxed);
        let (mut pos, mut reads) = (0usize, 0usize);
        while pos < model.len() {
            let mut b = vec![0u8; 1 + rnd(8) as usize];
            let n = h_read(fd, &mut b).unwrap();
            assert!(n > 0);
            assert_eq!(&b[..n], &model[pos..pos + n], "at {pos}");
            pos += n;
            reads += 1;
        }
        assert_eq!(h_read(fd, &mut [0u8; 4]), Ok(0));
        let queued = QUEUED.load(std::sync::atomic::Ordering::Relaxed) - q0;
        // (20 chunks of at most 5 blocks: a window per block at most, and the first)
        assert!(queued <= 20 * 5 + 1, "{queued} windows for {reads} reads");
        assert!(reads >= 3 * queued, "{queued} windows for {reads} reads");
        // a read inside the last block again: nothing fetched, nothing queued
        let fetched = || std::mem::take(&mut STORE.lock().unwrap().as_mut().unwrap().1);
        let wait_fetchers = || {
            let mut g = AHEAD.lock().unwrap_or_else(|e| e.into_inner());
            while g.as_ref().is_some_and(|a| !a.busy.is_empty()) {
                g = AHEAD_DONE.wait(g).unwrap_or_else(|e| e.into_inner());
            }
        };
        wait_fetchers();
        fetched();
        *CACHE.lock().unwrap() = None;
        let last = model.len() as u64 - 3;
        h_seek(fd, last as i64, libc::SEEK_SET).unwrap();
        let mut b = [0u8; 2];
        assert_eq!(h_read(fd, &mut b), Ok(2));
        assert_eq!(&b, &model[last as usize..last as usize + 2]);
        wait_fetchers();
        assert!(fetched().is_empty(), "the thread's last block, from the store again");
        // anywhere, any length, the cache sometimes emptied
        for k in 0..400 {
            if k % 37 == 0 {
                wait_fetchers();
                *CACHE.lock().unwrap() = None;
            }
            let off = rnd(model.len() as u64 + 30);
            let len = rnd(260) as usize;
            let mut b = vec![0u8; len];
            h_seek(fd, off as i64, libc::SEEK_SET).unwrap();
            let n = h_read(fd, &mut b).unwrap();
            let want = &model[(off as usize).min(model.len())..(off as usize + len).min(model.len())];
            assert_eq!(&b[..n], want, "{len} at {off}");
            // and on from there, as the sequential reader it may be
            let mut b2 = vec![0u8; rnd(30) as usize];
            let n2 = h_read(fd, &mut b2).unwrap();
            let at = (off as usize + n).min(model.len());
            assert_eq!(&b2[..n2], &model[at..(at + b2.len()).min(model.len())]);
        }
        wait_fetchers();
        release(fd);
        *LAST_END.lock().unwrap() = None;
    }

    // same_nocase is `k.to_lowercase() == name.to_lowercase()` (what name_nocase compared,
    // with a String per key), Σ, the Kelvin sign and İ included.
    proptest::proptest! {
        #[test]
        fn same_nocase_is_lowercase_eq(k in "[aAzZ_.0-9ΣσςİiKkÄäあア漢]{0,6}", n in "[aAzZ_.0-9ΣσςİiKkÄäあア漢]{0,6}") {
            let lc = n.to_lowercase();
            proptest::prop_assert_eq!(same_nocase(&k, &n, &lc), k.to_lowercase() == lc);
        }
    }

    #[test]
    fn same_nocase_cases() {
        let same = |k: &str, n: &str| same_nocase(k, n, &n.to_lowercase());
        assert!(same("Data.XP3", "data.xp3"));
        assert!(!same("data.xp", "data.xp3"));
        assert!(same("\u{212a}", "k"));
        assert!(same("ΟΔΟΣ", "οδος") == ("ΟΔΟΣ".to_lowercase() == "οδος"));
        assert!(same("ボイス", "ボイス") && !same("ボイス", "ぼいす"));
    }

    /// (a measurement, not a check: `cargo test --release -p kura --lib bench_small_reads --
    /// --ignored --nocapture`) ReadFile-sized reads of a store file whose blocks are all here:
    /// the per-read cost of the locks and allocations around the copy (review C-3).
    #[test]
    #[ignore]
    fn bench_small_reads() {
        let _g = TEST_FS.lock().unwrap_or_else(|e| e.into_inner());
        let h = |i: u32| format!("{:032x}", 0xbe4c_0000u128 + i as u128);
        let mut m = HashMap::new();
        let mut list = Vec::new();
        let mut size = 0;
        for i in 0..64u32 {
            m.insert(format!("chunks/{}/{}", &h(i)[..2], h(i)), vec![i as u8 + 1; 1000]);
            list.push(format!("{}:1000", h(i)));
            size += 1000;
        }
        let tsv = format!("vnvm-tree\t2\t64\nf\t644\t0\tbig.pck\t{size}\t{}\n", list.join(","));
        let mut fs = Fs { next_handle: 1, ..Default::default() };
        let root = fs.add(Data::Dir(BTreeMap::new()), 0o755, 0);
        // (not at "/": the other tests' files (video's temporary .vnv) stay outside it)
        fs.mounts.push(("/vfs-test".into(), root));
        fs.load_tree(root, &tsv);
        *FS.lock().unwrap() = Some(fs);
        *STORE.lock().unwrap() = Some((m, Vec::new()));
        *CACHE.lock().unwrap() = None;
        *LAST_END.lock().unwrap() = None;
        let At::Ours(Ok(fd)) = open_handle("/vfs-test/big.pck", libc::O_RDONLY, 0) else { panic!("open") };
        let mut b = [0u8; 16];
        // (everything fetched once)
        for _ in 0..2 {
            h_seek(fd, 0, libc::SEEK_SET).unwrap();
            while h_read(fd, &mut b).unwrap() > 0 {}
        }
        let wait = || {
            let mut g = AHEAD.lock().unwrap_or_else(|e| e.into_inner());
            while g.as_ref().is_some_and(|a| !a.busy.is_empty()) {
                g = AHEAD_DONE.wait(g).unwrap_or_else(|e| e.into_inner());
            }
        };
        wait();
        for (what, len) in [("16-byte reads", 16usize), ("4-byte reads", 4)] {
            let mut b = vec![0u8; len];
            let t = std::time::Instant::now();
            let mut n = 0u64;
            for _ in 0..20 {
                h_seek(fd, 0, libc::SEEK_SET).unwrap();
                while h_read(fd, &mut b).unwrap() > 0 {
                    n += 1;
                }
            }
            println!("BENCH vfs {what}: {:.0} ns per read ({n} reads)", t.elapsed().as_nanos() as f64 / n as f64);
        }
        wait();
        release(fd);
        *LAST_END.lock().unwrap() = None;
    }

    /// A read that waits for the network tells the page what the game reads soon: a voice the
    /// folder's next voices (once), an archive its next chunks (each chunk once); a read the
    /// cache has tells nothing; movies nothing.
    #[test]
    fn reads_hint_what_comes_next() {
        let _g = TEST_FS.lock().unwrap_or_else(|e| e.into_inner());
        let h = |i: u32| format!("{:032x}", 0xc0de_0000u128 + i as u128);
        let mut tsv = String::from("vnvm-tree\t2\t64\nd\t755\t0\tvoice\nd\t755\t0\tmovie\n");
        let mut m = HashMap::new();
        for i in 1..=20u32 {
            tsv += &format!("f\t644\t0\tvoice/v{i:03}.ogg\t48\t{}:48\n", h(i));
            m.insert(format!("chunks/{}/{}", &h(i)[..2], h(i)), vec![i as u8; 48]);
        }
        // (an archive of 10 chunks, a movie of 3)
        let arc: Vec<String> = (100..110).map(|i| format!("{}:48", h(i))).collect();
        tsv += &format!("f\t644\t0\tdata.xp3\t{}\t{}\n", 48 * 10, arc.join(","));
        let mov: Vec<String> = (200..203).map(|i| format!("{}:48", h(i))).collect();
        tsv += &format!("f\t644\t0\tmovie/op.mpg\t{}\t{}\n", 48 * 3, mov.join(","));
        for i in (100..110).chain(200..203) {
            m.insert(format!("chunks/{}/{}", &h(i)[..2], h(i)), vec![i as u8; 48]);
        }
        let mut fs = Fs { next_handle: 1, ..Default::default() };
        let root = fs.add(Data::Dir(BTreeMap::new()), 0o755, 0);
        fs.mounts.push(("/".into(), root));
        fs.load_tree(root, &tsv);
        *FS.lock().unwrap() = Some(fs);
        *STORE.lock().unwrap() = Some((m, Vec::new()));
        *CACHE.lock().unwrap() = None;
        *HINTED.lock().unwrap() = None;
        *LAST_END.lock().unwrap() = None;
        HINTS.lock().unwrap().clear();
        let name = |i: u32| format!("chunks/{}/{}", &h(i)[..2], h(i));
        let read = |p: &str, off: u64| {
            let At::Ours(Ok(fd)) = open_handle(p, libc::O_RDONLY, 0) else { panic!("open {p}") };
            let ino = handle(fd).unwrap().0;
            let mut b = [0u8; 8];
            assert_eq!(read_at(ino, &mut b, off), Ok(8));
            release(fd);
            std::mem::take(&mut *HINTS.lock().unwrap())
        };
        // (a voice: the next voices of its folder (all of them here), not itself nor those before it)
        assert_eq!(read("/voice/v005.ogg", 0), (6..=20).map(name).collect::<Vec<_>>());
        // (again: cached now, nothing)
        assert!(read("/voice/v005.ogg", 8).is_empty());
        // (a small archive: all of it at its first read but the chunk read; later, nothing again)
        assert_eq!(read("/data.xp3", 48 * 2), (100..110).filter(|&i| i != 102).map(name).collect::<Vec<_>>());
        assert!(read("/data.xp3", 48 * 7).is_empty());
        // (a movie: nothing)
        assert!(read("/movie/op.mpg", 0).is_empty());
    }

    /// A script the game reads names what comes next: a KiriKiri scenario inside an .xp3 (zlib'd,
    /// UTF-16) names a picture in another member, a voice loose in a folder and the next
    /// scenario, and those are foreseen, not what it does not name; a loose script the same; a
    /// script looked into once.
    #[test]
    fn scripts_say_what_comes_next() {
        let _g = TEST_FS.lock().unwrap_or_else(|e| e.into_inner());
        let z = |b: &[u8]| miniz_oxide::deflate::compress_to_vec_zlib(b, 6);
        let utf16 = |s: &str| [&[0xff, 0xfe][..], &s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect::<Vec<u8>>()].concat();
        // the archive: header, members, index (zlib'd) at the end
        let first = z(&utf16("*start\r\n[bg storage=\"bg01\"]\r\n@voice storage=v002\r\n「やあ」[jump storage=\"second.ks\"]"));
        let second = b"[bg storage=bg02]".to_vec();
        let (bg01, bg02) = (vec![1u8; 100], vec![2u8; 100]);
        let mut arc = b"XP3\r\n \n\x1a\x8b\x67\x01".to_vec();
        arc.extend(0u64.to_le_bytes());
        arc.resize(64, 0);
        let mut idx = Vec::new();
        let mut put = |arc: &mut Vec<u8>, name: &str, body: &[u8], zl: bool| {
            idx.extend(predict::xp3_entry(name, &[(arc.len() as u64, body.len() as u64, zl)]));
            arc.extend_from_slice(body);
            arc.resize(arc.len().div_ceil(64) * 64, 0);
        };
        put(&mut arc, "scenario/first.ks", &first, true);
        put(&mut arc, "bgimage/bg01.png", &bg01, false);
        put(&mut arc, "bgimage/bg02.png", &bg02, false);
        put(&mut arc, "scenario/second.ks", &second, false);
        let at = arc.len() as u64;
        arc[11..19].copy_from_slice(&at.to_le_bytes());
        let zi = z(&idx);
        arc.push(1);
        arc.extend((zi.len() as u64).to_le_bytes());
        arc.extend((idx.len() as u64).to_le_bytes());
        arc.extend(&zi);
        // 64-byte chunks, each its own hash
        let h = |i: usize| format!("{:032x}", 0x5c41_0000u128 + i as u128);
        let mut m = HashMap::new();
        let mut list = Vec::new();
        for (i, c) in arc.chunks(64).enumerate() {
            m.insert(format!("chunks/{}/{}", &h(i)[..2], h(i)), c.to_vec());
            list.push(format!("{}:{}", h(i), c.len()));
        }
        let mut tsv = format!("vnvm-tree\t2\t64\nd\t755\t0\tvoice\nf\t644\t0\tdata.xp3\t{}\t{}\n", arc.len(), list.join(","));
        for v in 1..=3usize {
            let k = 1000 + v;
            tsv += &format!("f\t644\t0\tvoice/v{v:03}.ogg\t48\t{}:48\n", h(k));
            m.insert(format!("chunks/{}/{}", &h(k)[..2], h(k)), vec![v as u8; 48]);
        }
        let loose = b"@se storage=v003 ; a loose script".to_vec();
        tsv += &format!("f\t644\t0\tscene.ks\t{}\t{}:{}\n", loose.len(), h(2000), loose.len());
        m.insert(format!("chunks/{}/{}", &h(2000)[..2], h(2000)), loose.clone());
        let mut fs = Fs { next_handle: 1, ..Default::default() };
        let root = fs.add(Data::Dir(BTreeMap::new()), 0o755, 0);
        fs.mounts.push(("/".into(), root));
        fs.load_tree(root, &tsv);
        *FS.lock().unwrap() = Some(fs);
        *STORE.lock().unwrap() = Some((m, Vec::new()));
        *CACHE.lock().unwrap() = None;
        *PREDICT.lock().unwrap() = None;
        let ino = |p: &str| {
            let At::Ours(Ok(fd)) = open_handle(p, libc::O_RDONLY, 0) else { panic!("open {p}") };
            let i = handle(fd).unwrap().0;
            release(fd);
            i
        };
        let (data, scene) = (ino("/data.xp3"), ino("/scene.ks"));
        // (what is named, in order, but what the reads' fetches ahead put on the device meanwhile)
        let same = |got: Vec<Hash>, want: Vec<Hash>| {
            let on = |h: &Hash| CACHE.lock().unwrap().as_ref().is_some_and(|c| c.contains(&(*h, 0)));
            let left: Vec<Hash> = want.iter().copied().filter(|h| got.contains(h) || !on(h)).collect();
            assert_eq!(got, left, "named {want:?}");
        };
        let chunk_of = |off: usize| {
            let x = h(off / 64);
            let mut b = [0u8; 16];
            for (k, c) in b.iter_mut().enumerate() {
                *c = u8::from_str_radix(&x[2 * k..2 * k + 2], 16).unwrap();
            }
            b
        };
        let hash = |i: usize| chunk_of(i * 64);
        // (the game reads the header: the index is read, nothing named yet)
        assert!(predicted(data, 0, 32).is_empty());
        // (the game reads first.ks: bg01's member, v002, second.ks; in that order)
        let starts: Vec<usize> = { let p = PREDICT.lock().unwrap(); let ms = &p.as_ref().unwrap().archives[&data]; ms.iter().filter(|m| predict::is_script(&m.name)).map(|m| m.segments[0].0 as usize).collect() };
        assert_eq!(starts.len(), 2, "two scripts in the archive");
        let got = predicted(data, starts[0] as u64, first.len() as u64);
        let v = |k: usize| hash(1000 + k);
        let bg01_at = 64 + first.len().div_ceil(64) * 64;
        same(got.clone(), vec![chunk_of(bg01_at), chunk_of(bg01_at + 64), v(2), chunk_of(starts[1])]);
        assert!(!got.contains(&v(1)) && !got.contains(&v(3)), "not what it does not name");
        // (again: looked into once)
        assert!(predicted(data, starts[0] as u64, 8).is_empty());
        // (second.ks names bg02)
        let bg02_at = bg01_at + 128;
        same(predicted(data, starts[1] as u64, second.len() as u64), vec![chunk_of(bg02_at), chunk_of(bg02_at + 64)]);
        // (a loose script)
        same(predicted(scene, 0, 4), vec![v(3)]);
        // (what is on the device already: not again)
        *PREDICT.lock().unwrap() = None;
        let mut c = Cache::default();
        c.put(&(v(3), 0), Arc::new(vec![3; 48]), false, 1 << 20);
        *CACHE.lock().unwrap() = Some(c);
        assert!(predicted(scene, 0, 4).is_empty());
        *CACHE.lock().unwrap() = None;
        *FS.lock().unwrap() = None;
    }

    /// A table read ahead (ippai serve's, of all a tree's scripts): a script this cannot read
    /// (encrypted: noise here) foresees what the table says it names, and what the script it
    /// goes on to names, before that one is read; without the table, nothing.
    #[test]
    fn a_table_foresees_scripts_not_read_yet() {
        let _g = TEST_FS.lock().unwrap_or_else(|e| e.into_inner());
        let noise: Vec<u8> = (0..200u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8 | 0x80).collect();
        let arc = predict::xp3_archive(&[("scenario/first.ks", &noise, false), ("scenario/second.ks", &noise, false), ("bg01.png", &[1; 50], false), ("bg02.png", &[2; 50], false), ("bg03.png", &[3; 50], false)]);
        let h = |i: usize| format!("{:032x}", 0xf0e5_0000u128 + i as u128);
        let mut m = HashMap::new();
        let mut list = Vec::new();
        for (i, c) in arc.chunks(64).enumerate() {
            m.insert(format!("chunks/{}/{}", &h(i)[..2], h(i)), c.to_vec());
            list.push(format!("{}:{}", h(i), c.len()));
        }
        let tsv = format!("vnvm-tree\t2\t64\nf\t644\t0\tdata.xp3\t{}\t{}\n", arc.len(), list.join(","));
        let mut fs = Fs { next_handle: 1, ..Default::default() };
        let root = fs.add(Data::Dir(BTreeMap::new()), 0o755, 0);
        fs.mounts.push(("/".into(), root));
        fs.load_tree(root, &tsv);
        *FS.lock().unwrap() = Some(fs);
        *STORE.lock().unwrap() = Some((m, Vec::new()));
        *CACHE.lock().unwrap() = None;
        *PREDICT.lock().unwrap() = None;
        let At::Ours(Ok(fd)) = open_handle("/data.xp3", libc::O_RDONLY, 0) else { panic!("open") };
        let data = handle(fd).unwrap().0;
        release(fd);
        let at = predict::xp3_index_at(&arc).unwrap() as usize;
        let ms = predict::xp3_index(&predict::xp3_index_bytes(&arc[at..]).unwrap());
        let (first, png) = (ms[0].segments[0], |k: usize| ms[2 + k].segments[0].0 as usize / 64);
        let hash = |i: usize| {
            let x = h(i);
            let mut b = [0u8; 16];
            for (k, c) in b.iter_mut().enumerate() {
                *c = u8::from_str_radix(&x[2 * k..2 * k + 2], 16).unwrap();
            }
            b
        };
        // (no table: the noise names nothing)
        assert!(predicted(data, 0, 32).is_empty());
        assert!(predicted(data, first.0, first.1).is_empty());
        *PREDICT.lock().unwrap() = None;
        foresee_table(&format!("{}\n{}{}", predict::TABLE_HEADER, predict::table_line("scenario/first.ks", &["bg01".into(), "second".into()]), predict::table_line("second.ks", &["bg03.png".into()])));
        predicted(data, 0, 32);
        let on = |x: &Hash| CACHE.lock().unwrap().as_ref().is_some_and(|c| c.contains(&(*x, 0)));
        let got = predicted(data, first.0, first.1);
        let (s2, n2) = (ms[1].segments[0].0 as usize, ms[1].segments[0].1 as usize);
        let second: Vec<Hash> = (s2 / 64..=(s2 + n2 - 1) / 64).map(hash).collect();
        let want: Vec<Hash> = std::iter::once(hash(png(0))).chain(second).chain([hash(png(2))]).filter(|x| got.contains(x) || !on(x)).collect();
        assert_eq!(got, want, "bg01, second.ks itself, then what second.ks names");
        assert!(!got.contains(&hash(png(1))), "not bg02, which nothing names");
        assert!(got.contains(&hash(png(2))));
        *CACHE.lock().unwrap() = None;
        *PREDICT.lock().unwrap() = None;
        // (no tree at "/" left for the tests of host files)
        *FS.lock().unwrap() = None;
    }

    /// A BGI .arc with a table (its scripts are bytecode, named without extension): a script
    /// member read foresees what the table says; a picture read mid-scene, what its scene
    /// names after it; a loose voice the same.
    #[test]
    fn bgi_archives_and_what_follows_a_file() {
        let _g = TEST_FS.lock().unwrap_or_else(|e| e.into_inner());
        // PackFile: 4 members of 64 bytes each after the index
        let names = ["01_start", "bg01a", "ch01", "ch02"];
        let mut arc = b"PackFile    ".to_vec();
        arc.extend((names.len() as u32).to_le_bytes());
        for (k, n) in names.iter().enumerate() {
            let mut e = vec![0u8; 32];
            e[..n.len()].copy_from_slice(n.as_bytes());
            e[16..20].copy_from_slice(&(k as u32 * 64).to_le_bytes());
            e[20..24].copy_from_slice(&64u32.to_le_bytes());
            arc.extend(e);
        }
        arc.resize(16 + 32 * 4, 0);
        let base = arc.len();
        for k in 0..names.len() {
            arc.extend(std::iter::repeat_n(k as u8 + 1, 64));
        }
        let h = |i: usize| format!("{:032x}", 0xb610_0000u128 + i as u128);
        let mut m = HashMap::new();
        let mut list = Vec::new();
        for (i, c) in arc.chunks(64).enumerate() {
            m.insert(format!("chunks/{}/{}", &h(i)[..2], h(i)), c.to_vec());
            list.push(format!("{}:{}", h(i), c.len()));
        }
        let mut tsv = format!("vnvm-tree\t2\t64\nf\t644\t0\tdata01000.arc\t{}\t{}\n", arc.len(), list.join(","));
        for v in 1..=2usize {
            tsv += &format!("f\t644\t0\tv{v:03}.ogg\t48\t{}:48\n", h(900 + v));
            m.insert(format!("chunks/{}/{}", &h(900 + v)[..2], h(900 + v)), vec![v as u8; 48]);
        }
        let mut fs = Fs { next_handle: 1, ..Default::default() };
        let root = fs.add(Data::Dir(BTreeMap::new()), 0o755, 0);
        fs.mounts.push(("/".into(), root));
        fs.load_tree(root, &tsv);
        *FS.lock().unwrap() = Some(fs);
        *STORE.lock().unwrap() = Some((m, Vec::new()));
        *CACHE.lock().unwrap() = None;
        *PREDICT.lock().unwrap() = None;
        let ino = |p: &str| {
            let At::Ours(Ok(fd)) = open_handle(p, libc::O_RDONLY, 0) else { panic!("open {p}") };
            let i = handle(fd).unwrap().0;
            release(fd);
            i
        };
        let (data, v1) = (ino("/data01000.arc"), ino("/v001.ogg"));
        let hash = |i: usize| {
            let x = h(i);
            let mut b = [0u8; 16];
            for (k, c) in b.iter_mut().enumerate() {
                *c = u8::from_str_radix(&x[2 * k..2 * k + 2], 16).unwrap();
            }
            b
        };
        // (a member's chunks: the index ends mid-chunk, so each member spans two)
        let member = |k: usize| ((base + 64 * k) / 64..=(base + 64 * k + 63) / 64).map(hash).collect::<Vec<Hash>>();
        let named = |parts: Vec<Vec<Hash>>| {
            let mut out: Vec<Hash> = Vec::new();
            for x in parts.into_iter().flatten() {
                if !out.contains(&x) {
                    out.push(x);
                }
            }
            out
        };
        let on = |x: &Hash| CACHE.lock().unwrap().as_ref().is_some_and(|c| c.contains(&(*x, 0)));
        let same = |got: Vec<Hash>, want: Vec<Hash>| {
            let left: Vec<Hash> = want.iter().copied().filter(|x| got.contains(x) || !on(x)).collect();
            assert_eq!(got, left);
        };
        foresee_table(&format!("{}\n{}", predict::TABLE_HEADER, predict::table_line("01_start", &["bg01a".into(), "v001".into(), "ch01".into(), "v002".into(), "ch02".into()])));
        // (the index read with the header; the script: all it names)
        assert!(predicted(data, 0, 16).is_empty());
        same(predicted(data, base as u64, 64), named(vec![member(1), vec![hash(901)], member(2), vec![hash(902)], member(3)]));
        // (a picture read: what its scene names after it)
        *PREDICT.lock().unwrap() = None;
        foresee_table(&format!("{}\n{}", predict::TABLE_HEADER, predict::table_line("01_start", &["bg01a".into(), "v001".into(), "ch01".into(), "v002".into(), "ch02".into()])));
        predicted(data, 0, 16);
        same(predicted(data, base as u64 + 64, 64), named(vec![vec![hash(901)], member(2), vec![hash(902)], member(3)]));
        // (a loose voice: what comes after it)
        same(predicted(v1, 0, 8), named(vec![member(2), vec![hash(902)], member(3)]));
        // (no table: a file that is no script says nothing)
        *PREDICT.lock().unwrap() = None;
        HAS_TABLE.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(predicted(v1, 0, 8).is_empty());
        *CACHE.lock().unwrap() = None;
        *FS.lock().unwrap() = None;
    }

    /// The run's trace: a path before its file's reads, each chunk's first read once (with the
    /// read's offset and wait), hints with their source; a prediction's own reads are not the
    /// game's; it parses back.
    #[test]
    fn the_trace_says_what_was_read_when() {
        let _g = TEST_FS.lock().unwrap_or_else(|e| e.into_inner());
        let h = |i: u32| format!("{:032x}", 0x7ace_0000u128 + i as u128);
        let mut m = HashMap::new();
        let mut tsv = String::from("vnvm-tree\t2\t64\nd\t755\t0\tvoice\n");
        for i in 1..=3u32 {
            tsv += &format!("f\t644\t0\tvoice/v{i:03}.ogg\t96\t{}:48,{}:48\n", h(i), h(i + 100));
            m.insert(format!("chunks/{}/{}", &h(i)[..2], h(i)), vec![i as u8; 48]);
            m.insert(format!("chunks/{}/{}", &h(i + 100)[..2], h(i + 100)), vec![i as u8; 48]);
        }
        let script = b"@voice storage=v003".to_vec();
        tsv += &format!("f\t644\t0\tfirst.ks\t{}\t{}:{}\n", script.len(), h(50), script.len());
        m.insert(format!("chunks/{}/{}", &h(50)[..2], h(50)), script.clone());
        let mut fs = Fs { next_handle: 1, ..Default::default() };
        let root = fs.add(Data::Dir(BTreeMap::new()), 0o755, 0);
        fs.mounts.push(("/".into(), root));
        fs.load_tree(root, &tsv);
        *FS.lock().unwrap() = Some(fs);
        *STORE.lock().unwrap() = Some((m, Vec::new()));
        *CACHE.lock().unwrap() = None;
        *PREDICT.lock().unwrap() = None;
        *HINTED.lock().unwrap() = None;
        // (a file's first read hints its folder: not the first when an earlier test's file had
        // this inode)
        *LAST_END.lock().unwrap() = None;
        trace::reset();
        let read = |p: &str, off: u64, n: usize| {
            let At::Ours(Ok(fd)) = open_handle(p, libc::O_RDONLY, 0) else { panic!("open {p}") };
            let ino = handle(fd).unwrap().0;
            let mut b = vec![0u8; n];
            assert_eq!(read_at(ino, &mut b, off), Ok(n));
            release(fd);
            ino
        };
        let v1 = read("/voice/v001.ogg", 40, 16);
        read("/voice/v001.ogg", 0, 8);
        let ks = read("/first.ks", 0, 4);
        let text = trace::text();
        assert!(text.starts_with("ippai-trace 1\n"));
        let lines = trace::parse(&text);
        let reads: Vec<(Ino, u64, u32, String)> = lines.iter().filter_map(|l| match l {
            trace::Line::Read { ino, off, chunk, hash, .. } => Some((*ino, *off, *chunk, hash.clone())),
            _ => None,
        }).collect();
        // (v001's two chunks once each, at the read's offset; first.ks; not v003, which only the
        // script's prediction read)
        assert_eq!(reads, [(v1, 40, 0, h(1)), (v1, 40, 1, h(101)), (ks, 0, 0, h(50))]);
        let pos = |f: &dyn Fn(&trace::Line) -> bool| lines.iter().position(f).unwrap();
        assert!(pos(&|l| matches!(l, trace::Line::Path { ino, path } if *ino == v1 && path == "/voice/v001.ogg")) < pos(&|l| matches!(l, trace::Line::Read { ino, .. } if *ino == v1)));
        let hints: Vec<(String, Vec<String>)> = lines.iter().filter_map(|l| match l {
            trace::Line::Hint { source, hashes, .. } => Some((source.clone(), hashes.clone())),
            _ => None,
        }).collect();
        assert!(hints.iter().any(|(s, hs)| s == "layout" && hs.contains(&h(2))), "{hints:?}");
        assert!(hints.iter().any(|(s, hs)| s == "script" && hs.contains(&h(3)) && hs.contains(&h(103))), "{hints:?}");
        // (times never go back)
        let ms: Vec<u64> = lines.iter().filter_map(|l| match l {
            trace::Line::Read { ms, .. } | trace::Line::Hint { ms, .. } => Some(*ms),
            _ => None,
        }).collect();
        assert!(ms.windows(2).all(|w| w[0] <= w[1]));
        *CACHE.lock().unwrap() = None;
        *FS.lock().unwrap() = None;
    }

    /// A padded get copies only a chunk someone else holds.
    #[test]
    fn no_fetcher_threads_no_wait() {
        // (a thread limit, little memory: a read must not wait on a queue no thread takes)
        SPAWN_FAILS.store(true, std::sync::atomic::Ordering::Relaxed);
        let h: Key = (*b"no-fetcher-test!", 0);
        fetch_ahead(Piece { h: h.0, clen: 10, first: 0, n: 1 });
        let busy = AHEAD.lock().unwrap().as_ref().is_some_and(|a| a.busy.contains(&h));
        SPAWN_FAILS.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(!busy, "queued with no thread to take it");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            wait_ahead(&h);
            let _ = tx.send(());
        });
        rx.recv_timeout(std::time::Duration::from_secs(5)).expect("wait_ahead waited for ever");
    }

    /// A block still queued when a read needs it is fetched by the read itself: while a save
    /// state has the fetchers stand still (the read waited for ever: the save failed, the
    /// thread could not stop), or behind another file's read-ahead (review b04).
    #[test]
    fn a_read_takes_its_queued_block_back() {
        let _fs = TEST_FS.lock().unwrap_or_else(|e| e.into_inner());
        // (the fetchers up (fetch_ahead takes ONE to bring them up), then standing still)
        fetch_ahead(Piece { h: *b"queued-back-up!!", clen: 10, first: 0, n: 1 });
        let _one = crate::snap::ONE.lock().unwrap_or_else(|e| e.into_inner());
        crate::snap::test_pause(true);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let h: Hash = *b"queued-back-blk!";
        let hx = hex(&h);
        STORE.lock().unwrap().get_or_insert_with(Default::default).0.insert(format!("chunks/{}/{hx}", &hx[..2]), vec![7u8; 100]);
        {
            let mut g = AHEAD.lock().unwrap();
            let a = g.as_mut().unwrap();
            a.busy.insert((h, 0));
            a.queue.push_back(Piece { h, clen: 100, first: 0, n: 1 });
        }
        let before = TAKEN_BACK.load(std::sync::atomic::Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            wait_ahead(&(h, 0));
            let _ = tx.send(());
        });
        let r = rx.recv_timeout(std::time::Duration::from_secs(5));
        crate::snap::test_pause(false);
        r.expect("a read waited for a block no fetcher could take");
        assert_eq!(TAKEN_BACK.load(std::sync::atomic::Ordering::SeqCst), before + 1);
        assert!(cached(&(h, 0)));
        let name = STORE.lock().unwrap().as_ref().unwrap().1.iter().rev().find(|n| n.contains(&hx)).cloned().unwrap();
        assert!(!name.ends_with("!a"), "fetched at the read's priority: {name}");
    }

    #[test]
    fn fetcher_start_check_releases_ahead_before_snap_lock() {
        let _ = needs_fetchers();
        // No temporary AHEAD guard may survive until the caller takes snap::ONE.
        // A snap test owns ONE while its fetchers need AHEAD to stand still.
        assert!(AHEAD.lock().is_ok());
    }

    /// A file in memory at offsets a 32-bit usize does not hold (wasm32; `max` stands for its
    /// isize::MAX here): the end is the 64-bit sum or EFBIG, never the offset's low half.
    #[test]
    fn upper_end_does_not_wrap() {
        const MAX32: usize = i32::MAX as usize;
        assert_eq!(upper_end(0, 0, MAX32), Ok(0));
        assert_eq!(upper_end(10, 5, MAX32), Ok(15));
        assert_eq!(upper_end(MAX32 as u64 - 5, 5, MAX32), Ok(MAX32));
        assert_eq!(upper_end(MAX32 as u64 - 4, 5, MAX32), Err(libc::EFBIG));
        // (`off as usize` on wasm32: 0x1_0000_0010 was 0x10, and 0xffff_fff0 + 0x20 wrapped to 0x10)
        assert_eq!(upper_end(0x1_0000_0010, 4, MAX32), Err(libc::EFBIG));
        assert_eq!(upper_end(0xffff_fff0, 0x20, MAX32), Err(libc::EFBIG));
        assert_eq!(upper_end(u64::MAX, 1, MAX32), Err(libc::EFBIG));
        assert_eq!(upper_end(u64::MAX, 1, usize::MAX), Err(libc::EFBIG));
    }

    /// A write or a new size the memory has no room for is an error (ENOSPC, EFBIG), the file
    /// as it was; reads past the end of a file in memory read nothing, whatever the offset.
    #[test]
    fn upper_file_too_big_is_an_error() {
        // (a file with no name: the tests share one file system)
        let ino = with(|fs| fs.add(Data::File(Content::Upper(b"abc".to_vec())), 0o644, 0));
        let size = |ino| with(|fs| fs.file_size(ino));
        // (more than any allocator has: not the whole address space zeroed first)
        let far = isize::MAX as u64 - 8;
        assert_eq!(write_at(ino, b"x", far), Err(libc::ENOSPC));
        assert_eq!(set_size(ino, far), Err(libc::ENOSPC));
        assert_eq!(write_at(ino, b"x", u64::MAX - 4), Err(libc::EFBIG));
        assert_eq!(set_size(ino, u64::MAX), Err(libc::EFBIG));
        assert_eq!(size(ino), 3);
        let mut b = [0u8; 4];
        assert_eq!(read_at(ino, &mut b, 1), Ok(2));
        assert_eq!(&b[..2], b"bc");
        assert_eq!(read_at(ino, &mut b, 0x1_0000_0001), Ok(0));
        assert_eq!(read_at(ino, &mut b, u64::MAX), Ok(0));
        // and the ordinary ones as before
        assert_eq!(write_at(ino, b"xy", 5), Ok(2));
        assert_eq!(size(ino), 7);
        assert_eq!(set_size(ino, 2), Ok(()));
        assert_eq!(set_size(ino, 4), Ok(()));
        let mut b = [9u8; 8];
        assert_eq!(read_at(ino, &mut b, 0), Ok(4));
        assert_eq!(&b[..4], b"ab\0\0");
    }

    #[test]
    fn cache_pads_in_place() {
        let mut c = Cache::default();
        let h = [1u8; 16];
        c.put(&h, Arc::new(vec![7; 10]), false, 1000);
        let p = Arc::as_ptr(&c.map[&h].data);
        let d = c.get(&h, 20, 1000).unwrap();
        assert_eq!(Arc::as_ptr(&d), p);
        assert_eq!((&d[..10], &d[10..], c.bytes), (&[7u8; 10][..], &[0u8; 10][..], 20));
        let d2 = c.get(&h, 30, 1000).unwrap();
        assert_ne!(Arc::as_ptr(&d2), Arc::as_ptr(&d));
        assert_eq!((d.len(), d2.len(), c.bytes), (20, 30, 30));
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::collection::vec;
    use proptest::prelude::*;

    proptest! {
        /// upper_end against the sum in 128 bits, for a 32-bit and a 64-bit usize's limit.
        #[test]
        fn upper_end_is_the_wide_sum(off in any::<u64>(), len in 0usize..=u32::MAX as usize, wide in any::<bool>(), near in any::<bool>(), back in 0u64..64) {
            let max = if wide { isize::MAX as usize } else { i32::MAX as usize };
            // (half the cases around the limit)
            let off = if near { (max as u64).saturating_sub(len as u64).wrapping_add(32).wrapping_sub(back) } else { off };
            let sum = off as u128 + len as u128;
            match upper_end(off, len, max) {
                Ok(e) => { prop_assert_eq!(e as u128, sum); prop_assert!(e <= max); }
                Err(e) => { prop_assert_eq!(e, libc::EFBIG); prop_assert!(sum > max as u128); }
            }
        }
    }

    /// `ippai pack`'s escaping for the tree list (crates/ippai/src/pack.rs).
    fn esc(s: &str) -> String {
        s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n")
    }

    fn mounted() -> (Fs, Ino) {
        let mut fs = Fs { next_handle: 1, ..Default::default() };
        let root = fs.add(Data::Dir(BTreeMap::new()), 0o755, 0);
        fs.mounts.push(("/".into(), root));
        (fs, root)
    }

    /// A saved file's place: up to two directory levels, a file name (never one of the
    /// directory names, so no path is both).
    fn save_path() -> impl Strategy<Value = String> {
        (vec(prop::sample::select(vec!["a", "b", "セーブ", "dir with space"]), 0..3), prop::sample::select(vec!["x.sav", "y.dat", "global.sav", "名前.bin"]))
            .prop_map(|(dirs, f)| dirs.iter().map(|d| format!("{d}/")).collect::<String>() + f)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// The saves the page keeps (Fs::export) read back as they were written, the last
        /// write of a path winning.
        #[test]
        fn saves_round_trip(files in vec((save_path(), vec(any::<u8>(), 0..300), any::<i64>()), 0..16)) {
            let (mut fs, _) = mounted();
            let mut want = BTreeMap::new();
            for (p, data, mtime) in &files {
                fs.put_file(&format!("/save/{p}"), data.clone(), *mtime);
                want.insert(format!("/save/{p}"), (data.clone(), *mtime));
            }
            let mut out = Vec::new();
            fs.export("/save", &mut out);
            let got: BTreeMap<String, (Vec<u8>, i64)> = parse_records(&out).into_iter().map(|(p, d, m)| (p, (d, m))).collect();
            prop_assert_eq!(got.len(), parse_records(&out).len(), "a path exported twice");
            prop_assert_eq!(got, want);
        }

        /// A truncated or corrupt saves blob gives some of its files, never a panic.
        #[test]
        fn saves_blob_any_bytes(b in vec(any::<u8>(), 0..200), cut in any::<prop::sample::Index>()) {
            let _ = parse_records(&b);
            let (mut fs, _) = mounted();
            fs.put_file("/s/a", b.clone(), 1);
            let mut out = Vec::new();
            fs.export("/s", &mut out);
            let n = cut.index(out.len() + 1);
            let got = parse_records(&out[..n]);
            prop_assert!(got.len() <= 1);
            prop_assert_eq!(got.len() == 1, n == out.len());
        }

        /// clean(): absolute, no "." / ".." / empty parts, and a fixed point.
        #[test]
        fn clean_is_canonical(parts in vec(prop::sample::select(vec!["", ".", "..", "a", "b", "日本", "c d", "..."]), 0..10), abs in any::<bool>()) {
            let p = (if abs { "/" } else { "" }).to_string() + &parts.join("/");
            let c = clean(&p);
            prop_assert!(c.starts_with('/'));
            prop_assert!(c == "/" || !c.ends_with('/'));
            prop_assert!(c[1..].split('/').all(|s| c == "/" || !matches!(s, "" | "." | "..")));
            prop_assert_eq!(clean(&c), c.clone());
        }

        /// A tree list with names holding tabs, newlines and backslashes: every entry is found
        /// under its own name.
        #[test]
        fn tree_names_escape(names in vec("[a-z\\\\\t\n ]{1,6}", 1..8), mode in 0u32..0o1000, mtime in any::<i32>()) {
            let (mut fs, root) = mounted();
            let mut tsv = String::from("vnvm-tree\t2\t262144\n");
            let mut want = BTreeMap::new();
            for (i, n) in names.iter().enumerate() {
                let n = format!("{i}{n}");
                let chunks = if i == 0 { String::new() } else { format!(":{}", i * 7) };
                tsv += &format!("f\t{mode:o}\t{mtime}\t{}\t{}\t{chunks}\n", esc(&format!("d/{n}")), i * 7);
                want.insert(n, i as u64 * 7);
            }
            tsv = format!("vnvm-tree\t2\t262144\nd\t755\t0\td\n{}", &tsv["vnvm-tree\t2\t262144\n".len()..]);
            fs.load_tree(root, &tsv);
            for (n, size) in want {
                let ino = fs.walk(root, "d");
                prop_assert!(ino.is_some());
                let f = match &fs.node(ino.unwrap()).unwrap().data { Data::Dir(ch) => ch.get(&n).copied(), _ => None };
                prop_assert!(f.is_some(), "{:?} not found", n);
                prop_assert_eq!(fs.file_size(f.unwrap()), size);
                prop_assert_eq!(fs.node(f.unwrap()).unwrap().mode, mode);
            }
        }

        /// Any text as a tree list: no panic.
        #[test]
        fn tree_list_any_text(t in "(([dfl]\t[0-7]{0,4}\t-?[0-9]{0,3}\t[a-z/\\\\.]{0,8}(\t[0-9a-f,]{0,40}){0,3})?\n){0,12}") {
            let (mut fs, root) = mounted();
            fs.load_tree(root, &t);
        }
    }

    /// The one file system for the namespace tests: "/" empty, and a mount at "/m".
    fn fresh_tree() -> std::sync::MutexGuard<'static, ()> {
        let g = super::tests::TEST_FS.lock().unwrap_or_else(|e| e.into_inner());
        let mut fs = Fs { next_handle: 1, ..Default::default() };
        let root = fs.add(Data::Dir(BTreeMap::new()), 0o755, 0);
        let m = fs.add(Data::Dir(BTreeMap::new()), 0o755, 0);
        if let Some(Data::Dir(ch)) = fs.data_mut(root) {
            ch.insert("m".into(), m);
        }
        if let Some(n) = fs.node_mut(root) {
            n.nlink += 1;
        }
        fs.mounts.push(("/".into(), root));
        fs.mounts.push(("/m".into(), m));
        *FS.lock().unwrap_or_else(|e| e.into_inner()) = Some(fs);
        g
    }

    /// The tree's own rules: each directory's links are 2 and one per directory in it, every
    /// name leads to a live node, no node has two names, and every live node is named (or
    /// open, or a mount's root): no leak, no name reaching a freed or reused inode.
    fn tree_holds() -> Result<(), String> {
        with(|fs| {
            let roots: Vec<Ino> = fs.mounts.iter().map(|m| m.1).collect();
            let mut seen = std::collections::HashSet::new();
            let mut stack = vec![(roots[0], "/".to_string())];
            while let Some((d, path)) = stack.pop() {
                if !seen.insert(d) {
                    return Err(format!("{path}: inode {d} has two names"));
                }
                let Some(n) = fs.node(d) else { return Err(format!("{path}: inode {d} was freed")) };
                if let Data::Dir(ch) = &n.data {
                    let subdirs = ch.values().filter(|&&c| matches!(fs.node(c).map(|n| &n.data), Some(Data::Dir(_)))).count() as u32;
                    if n.nlink != 2 + subdirs {
                        return Err(format!("{path}: nlink {} with {subdirs} directories in it", n.nlink));
                    }
                    for (name, &c) in ch {
                        stack.push((c, format!("{}/{name}", path.trim_end_matches('/'))));
                    }
                }
            }
            for (i, n) in fs.nodes.iter().enumerate() {
                if let Some(n) = n {
                    if !seen.contains(&(i as Ino)) && n.open_refs == 0 && !roots.contains(&(i as Ino)) {
                        return Err(format!("inode {i} is live, unnamed and not open"));
                    }
                }
            }
            Ok(())
        })
    }

    /// MoveFile of a directory, RemoveDirectory of it, then a handle on its old parent closed:
    /// the parent was freed while still named (its links were not moved) and the next node
    /// took its inode (review b04).
    #[test]
    fn moved_directories_keep_their_parents() {
        let _g = fresh_tree();
        for p in ["/tmp", "/tmp/d", "/b"] {
            assert!(matches!(mkdir_at(p), At::Ours(Ok(()))), "{p}");
        }
        assert!(matches!(rename_at("/tmp/d", "/b/d"), At::Ours(Ok(()))));
        assert!(matches!(remove_at("/b/d", true), At::Ours(Ok(()))));
        let At::Ours(Ok(h)) = open_handle("/b", libc::O_RDONLY, 0) else { panic!() };
        release(h);
        tree_holds().unwrap();
        assert!(matches!(mkdir_at("/c"), At::Ours(Ok(()))));
        assert!(matches!(info_at("/b", true), At::Ours(Ok(i)) if i.dir));
        assert_eq!(list_at("/b").ok().map(|v| v.len()), Some(0));
        // (into itself: refused; a mount's root stays)
        assert!(matches!(mkdir_at("/b/e"), At::Ours(Ok(()))));
        assert!(matches!(rename_at("/b", "/b/e/x"), At::Ours(Err(libc::EINVAL))));
        assert!(matches!(rename_at("/m", "/n"), At::Ours(Err(libc::EBUSY))));
        assert!(matches!(rename_at("/c", "/m"), At::Ours(Err(libc::EBUSY))));
        // (a file onto a directory, a directory onto a file)
        let At::Ours(Ok(h)) = open_handle("/f", libc::O_CREAT | libc::O_RDWR, 0o644) else { panic!() };
        release(h);
        assert!(matches!(rename_at("/f", "/c"), At::Ours(Err(libc::EISDIR))));
        assert!(matches!(rename_at("/c", "/f"), At::Ours(Err(libc::ENOTDIR))));
        tree_holds().unwrap();
    }

    impl<T> At<T> {
        fn ok(self) -> Option<T> {
            match self {
                At::Ours(Ok(t)) => Some(t),
                _ => None,
            }
        }
    }

    /// Threads creating one missing file at once: one node, every handle on it, what each
    /// wrote in it (two nodes were made, the first named nowhere for ever).
    #[test]
    fn concurrent_creates_make_one_file() {
        let _g = fresh_tree();
        for round in 0..50 {
            let p = format!("/log{round}");
            let hs: Vec<u32> = std::thread::scope(|s| {
                let t: Vec<_> = (0..4).map(|_| s.spawn(|| open_handle(&p, libc::O_CREAT | libc::O_RDWR, 0o644).ok().unwrap())).collect();
                t.into_iter().map(|t| t.join().unwrap()).collect()
            });
            let inos: std::collections::HashSet<Ino> = hs.iter().map(|&h| h_info(h).unwrap().ino).collect();
            assert_eq!(inos.len(), 1, "round {round}: {} nodes", inos.len());
            hs.iter().for_each(|&h| release(h));
        }
        tree_holds().unwrap();
    }

    mod namespace_model {
        use super::*;

        #[derive(Clone, Debug)]
        enum Op {
            Mkdir(String),
            Rmdir(String),
            Create(String),
            Unlink(String),
            Rename(String, String),
            Hold(String),
            Drop(usize),
        }

        fn path() -> impl Strategy<Value = String> {
            prop::collection::vec(prop::sample::select(vec!["a", "b", "c"]), 1..4).prop_map(|v| format!("/{}", v.join("/")))
        }

        fn op() -> impl Strategy<Value = Op> {
            prop_oneof![
                path().prop_map(Op::Mkdir),
                path().prop_map(Op::Rmdir),
                path().prop_map(Op::Create),
                path().prop_map(Op::Unlink),
                (path(), path()).prop_map(|(a, b)| Op::Rename(a, b)),
                path().prop_map(Op::Hold),
                (0usize..4).prop_map(Op::Drop),
            ]
        }

        fn parent(p: &str) -> String {
            match p.rsplit_once('/') {
                Some(("", _)) | None => "/".into(),
                Some((d, _)) => d.into(),
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(300))]
            /// Any sequence of mkdir, rmdir, create, unlink, rename and handles held across them:
            /// the same successes and the same names as a model of the tree, and the tree's own
            /// rules (links, live names, no leak) after every step.
            #[test]
            fn as_the_model(ops in prop::collection::vec(op(), 1..40)) {
                let _g = fresh_tree();
                // (path -> is a directory; "/" and the mount "/m" are there from the start)
                let mut m: BTreeMap<String, bool> = BTreeMap::from([("/".to_string(), true), ("/m".to_string(), true)]);
                let mut held: Vec<u32> = Vec::new();
                for o in &ops {
                    let is_dir = |m: &BTreeMap<String, bool>, p: &str| m.get(p) == Some(&true);
                    let empty = |m: &BTreeMap<String, bool>, p: &str| !m.keys().any(|k| k.len() > p.len() && k.starts_with(p) && k.as_bytes()[p.len()] == b'/');
                    let (want, got) = match o {
                        Op::Mkdir(p) => {
                            let w = is_dir(&m, &parent(p)) && !m.contains_key(p);
                            if w { m.insert(p.clone(), true); }
                            (w, mkdir_at(p).ok().is_some())
                        }
                        Op::Rmdir(p) => {
                            let w = is_dir(&m, p) && empty(&m, p) && p != "/m";
                            if w { m.remove(p); }
                            (w, if p == "/m" { w } else { remove_at(p, true).ok().is_some() })
                        }
                        Op::Create(p) => {
                            let w = is_dir(&m, &parent(p)) && m.get(p) != Some(&true);
                            if w { m.insert(p.clone(), false); }
                            let h = open_handle(p, libc::O_CREAT | libc::O_RDWR, 0o644).ok();
                            if let Some(h) = h { release(h); }
                            (w, h.is_some())
                        }
                        Op::Unlink(p) => {
                            let w = m.get(p) == Some(&false);
                            if w { m.remove(p); }
                            (w, remove_at(p, false).ok().is_some())
                        }
                        Op::Rename(a, b) => {
                            let w = match (m.get(a.as_str()).copied(), m.get(b.as_str()).copied()) {
                                _ if a == "/m" || b == "/m" => false,
                                (None, _) => false,
                                _ if a == b => true,
                                (Some(true), _) if b.starts_with(&format!("{a}/")) => false,
                                (Some(ad), bt) => is_dir(&m, &parent(b)) && match bt {
                                    None => true,
                                    Some(true) => ad && empty(&m, b),
                                    Some(false) => !ad,
                                },
                            };
                            if w && a != b {
                                let moved: Vec<(String, bool)> = m.iter().filter(|(k, _)| *k == a || k.starts_with(&format!("{a}/"))).map(|(k, &d)| (k.clone(), d)).collect();
                                m.remove(b.as_str());
                                for (k, d) in moved {
                                    m.remove(&k);
                                    m.insert(format!("{b}{}", &k[a.len()..]), d);
                                }
                            }
                            (w, rename_at(a, b).ok().is_some())
                        }
                        Op::Hold(p) => {
                            let w = m.contains_key(p);
                            let h = open_handle(p, libc::O_RDONLY, 0).ok();
                            if let Some(h) = h { held.push(h); }
                            (w, h.is_some())
                        }
                        Op::Drop(i) => {
                            if *i < held.len() { release(held.remove(*i)); }
                            (true, true)
                        }
                    };
                    prop_assert_eq!(got, want, "{:?}", o);
                    if let Err(e) = tree_holds() { prop_assert!(false, "after {:?}: {}", o, e); }
                    // (every name of the model, nothing else)
                    let mut names = BTreeMap::new();
                    let mut stack = vec!["/".to_string()];
                    while let Some(d) = stack.pop() {
                        names.insert(d.clone(), true);
                        for (n, i) in list_at(&d).ok().unwrap() {
                            let p = format!("{}/{n}", d.trim_end_matches('/'));
                            if i.dir { stack.push(p); } else { names.insert(p, false); }
                        }
                    }
                    prop_assert_eq!(&names, &m, "after {:?}", o);
                }
                held.into_iter().for_each(release);
                if let Err(e) = tree_holds() { prop_assert!(false, "at the end: {}", e); }
            }
        }
    }

    /// Saved-game records whose name length is near 4 GiB stop the parse, never index past the
    /// blob (16 + n wrapped on wasm32 and passed the length check).
    #[test]
    fn records_with_huge_lengths() {
        for n in [u32::MAX, u32::MAX - 15, u32::MAX - 3, 0x8000_0000] {
            let mut b = n.to_le_bytes().to_vec();
            b.extend([0u8; 40]);
            assert!(parse_records(&b).is_empty());
        }
        let mut b = 1u32.to_le_bytes().to_vec();
        b.push(b'x');
        b.extend(0i64.to_le_bytes());
        b.extend((u32::MAX - 20).to_le_bytes());
        b.extend([0u8; 30]);
        assert!(parse_records(&b).is_empty());
    }
}
