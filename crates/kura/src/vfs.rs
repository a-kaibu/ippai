//! The page's file system: directory trees packed by `ippai pack` (`trees/<name>.tsv` plus
//! chunks in the store) mounted where `IPPAI_MOUNTS` says ("/=:/vnvm/game=reunion"), read
//! lazily chunk by chunk. Whatever is written stays in memory. Paths outside the mounts are the
//! host's (node; the page mounts an empty tree at "/", so there is nothing outside). Rust code
//! reaches it through crate::fs.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::{Arc, Mutex};

use libc::size_t;

type Ino = u32;
type Hash = [u8; 16];

enum Content {
    /// From the store; `None` chunks are all zero.
    Lower {
        size: u64,
        chunk: u32,
        chunks: Arc<Vec<Option<Hash>>>,
    },
    Upper(Vec<u8>),
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
}

static FS: Mutex<Option<Fs>> = Mutex::new(None);

/// Records as `Fs::export` makes them.
fn parse_records(mut b: &[u8]) -> Vec<(String, Vec<u8>, i64)> {
    let mut files = Vec::new();
    let u32_at = |b: &[u8]| u32::from_le_bytes(b[..4].try_into().unwrap()) as usize;
    while b.len() >= 4 {
        let n = u32_at(b);
        if b.len() < 16 + n {
            break;
        }
        let path = String::from_utf8_lossy(&b[4..4 + n]).into_owned();
        let mtime = i64::from_le_bytes(b[4 + n..12 + n].try_into().unwrap());
        let size = u32_at(&b[12 + n..]);
        if b.len() < 16 + n + size {
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
    CHANGES.load(std::sync::atomic::Ordering::Relaxed)
}

#[no_mangle]
pub unsafe extern "C" fn vnhost_vfs_export(roots: *const c_char, len: *mut size_t) -> *mut u8 {
    let roots = CStr::from_ptr(roots).to_string_lossy().into_owned();
    let mut out = Vec::new();
    {
        let g = FS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(fs) = g.as_ref() {
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
    f(g.as_mut().unwrap())
}

static LOADING: Mutex<()> = Mutex::new(());

extern "C" {
    /// host/vnfs.js: reads `name` from the store into a malloc'ed buffer (NULL if missing)
    fn vnvm_fetch(name: *const c_char, len: *mut size_t) -> *mut u8;
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
/// last call (ms), the chunk cache (bytes).
#[no_mangle]
pub unsafe extern "C" fn vnhost_net_stats(out: *mut f64) {
    use std::sync::atomic::Ordering::Relaxed;
    let ms = |us: u64| us as f64 / 1000.0;
    let cache = CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map_or(0, |c| c.bytes);
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
            eprintln!(
                "vfs: fetch {name}: {}",
                if p.is_null() { -1 } else { len as isize }
            );
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
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Fs {
    fn load() -> Fs {
        let mut fs = Fs {
            next_handle: 1,
            ..Default::default()
        };
        let Ok(spec) = std::env::var("IPPAI_MOUNTS") else {
            return fs;
        };
        let mut mounts: Vec<(String, &str)> = spec
            .split(':')
            .filter_map(|m| m.split_once('='))
            .map(|(at, name)| (clean(at), name))
            .collect();
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
            .filter(|(at, _)| {
                path == at || path.starts_with(&format!("{}/", at.trim_end_matches('/')))
            })
            .max_by_key(|(at, _)| at.len())
            .map(|(at, ino)| (*ino, &path[at.len()..]))
    }

    /// Writes a whole file (and its directories) into a mounted tree.
    fn put_file(&mut self, path: &str, data: Vec<u8>, mtime: i64) {
        let Some((mut at, rel)) = self.split_mount(path) else {
            return;
        };
        let parts: Vec<&str> = rel.split('/').filter(|c| !c.is_empty()).collect();
        let Some((name, dirs)) = parts.split_last() else {
            return;
        };
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
        let Some((at, rel)) = self.split_mount(path) else {
            return;
        };
        let Some(ino) = self.walk(at, rel) else {
            return;
        };
        let mut todo = vec![(ino, path.trim_end_matches('/').to_string())];
        while let Some((ino, p)) = todo.pop() {
            let Some(n) = self.node(ino) else { continue };
            match &n.data {
                Data::Dir(ch) => {
                    todo.extend(ch.iter().map(|(name, &i)| (i, format!("{p}/{name}"))))
                }
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
        let chunk: u32 = lines
            .next()
            .and_then(|h| h.split('\t').nth(2)?.parse().ok())
            .unwrap_or(1 << 18);
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
            let Some(parent) = self.walk(root, dir) else {
                continue;
            };
            let data = match f[0] {
                "d" => Data::Dir(BTreeMap::new()),
                "l" if f.len() > 4 => Data::Link(unesc(f[4])),
                "f" if f.len() > 5 => {
                    let size = f[4].parse().unwrap_or(0);
                    let chunks = if f[5].is_empty() {
                        Vec::new()
                    } else {
                        f[5].split(',').map(hex16).collect()
                    };
                    Data::File(Content::Lower {
                        size,
                        chunk,
                        chunks: Arc::new(chunks),
                    })
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
        let n = Node {
            nlink: if matches!(data, Data::Dir(_)) { 2 } else { 1 },
            data,
            mode,
            mtime,
        };
        if let Some(i) = self.free.pop() {
            self.nodes[i as usize] = Some(n);
            return i;
        }
        self.nodes.push(Some(n));
        (self.nodes.len() - 1) as Ino
    }

    fn node(&self, ino: Ino) -> Option<&Node> {
        self.nodes.get(ino as usize)?.as_ref()
    }
    fn node_mut(&mut self, ino: Ino) -> Option<&mut Node> {
        CHANGES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.nodes.get_mut(ino as usize)?.as_mut()
    }
    fn data_mut(&mut self, ino: Ino) -> Option<&mut Data> {
        Some(&mut self.node_mut(ino)?.data)
    }

    fn mount_at(&self, path: &str) -> Option<Ino> {
        self.mounts.iter().find(|(p, _)| p == path).map(|m| m.1)
    }

    /// Resolves an absolute path.
    fn resolve(&self, path: &str, follow_last: bool) -> Result<Found, c_int> {
        let mut todo: VecDeque<String> = path
            .split('/')
            .filter(|c| !c.is_empty() && *c != ".")
            .map(String::from)
            .collect();
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
            let Some(Node {
                data: Data::Dir(ch),
                ..
            }) = self.node(dir)
            else {
                return Err(libc::ENOTDIR);
            };
            let Some(&child) = ch.get(&c) else {
                if last {
                    return Ok(Found::Missing {
                        parent: dir,
                        name: c,
                    });
                }
                return Err(libc::ENOENT);
            };
            if let Some(Node {
                data: Data::Link(target),
                ..
            }) = self.node(child)
            {
                if !last || follow_last {
                    links += 1;
                    if links > 40 {
                        return Err(libc::ELOOP);
                    }
                    if target.starts_with('/') {
                        cur.clear();
                        node = self.mount_at("/");
                    }
                    for (i, t) in target
                        .split('/')
                        .filter(|t| !t.is_empty() && *t != ".")
                        .enumerate()
                    {
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
        id
    }

    fn unlink_name(&mut self, parent: Ino, name: &str) {
        let Some(Data::Dir(ch)) = self.data_mut(parent) else {
            return;
        };
        let Some(ino) = ch.remove(name) else { return };
        if let Some(n) = self.node_mut(ino) {
            n.nlink = n.nlink.saturating_sub(1);
        }
        self.gc(ino);
    }

    /// Frees a node once no name or open handle refers to it.
    fn gc(&mut self, ino: Ino) {
        let linked = self
            .node(ino)
            .is_some_and(|n| n.nlink > 0 && !(matches!(n.data, Data::Dir(_)) && n.nlink < 2));
        if !linked && !self.handles.values().any(|h| h.ino == ino) {
            if let Some(slot @ Some(_)) = self.nodes.get_mut(ino as usize) {
                *slot = None;
                self.free.push(ino);
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

/// Chunks by hash, the oldest put dropped first once they hold more than the limit. `bytes`
/// is the sum of the lengths of what `map` holds: every change goes through [`Cache::put`].
#[derive(Default)]
struct Cache {
    map: HashMap<Hash, Arc<Vec<u8>>>,
    /// each hash in `map` once, oldest first
    order: VecDeque<Hash>,
    bytes: usize,
}

impl Cache {
    /// Holds `data` as `h` (in place of what `h` held: it keeps its place in the order), then
    /// drops the oldest while over `limit` (`h` itself last).
    fn put(&mut self, h: &Hash, data: Arc<Vec<u8>>, limit: usize) {
        match self.map.insert(*h, data.clone()) {
            Some(old) => self.bytes = self.bytes - old.len() + data.len(),
            None => {
                self.order.push_back(*h);
                self.bytes += data.len();
            }
        }
        self.trim(limit);
    }

    fn trim(&mut self, limit: usize) {
        while self.bytes > limit {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some(d) = self.map.remove(&old) {
                self.bytes -= d.len();
            }
        }
    }

    /// Chunk `h` at `len` bytes: one the page fetched ahead comes as stored (a file's last
    /// chunk short) and is padded in place (a copy only if someone holds it).
    fn get(&mut self, h: &Hash, len: usize, limit: usize) -> Option<Arc<Vec<u8>>> {
        let c = self.map.get_mut(h)?;
        let had = c.len();
        if had != len {
            Arc::make_mut(c).resize(len, 0);
            self.bytes = self.bytes - had + len;
        }
        let d = c.clone();
        self.trim(limit);
        Some(d)
    }
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

fn cache_limit() -> usize {
    static LIMIT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::env::var("IPPAI_CHUNK_CACHE_MB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(128usize)
            << 20
    })
}

/// The chunks this run read, in the order of their first read (the first FETCHED_MAX): the
/// next run's page fetches them ahead, several at a time (see jit::maybe_save). Here each is
/// one synchronous request. (Chunks the page fetched ahead count too: else a run that had
/// all it read fetched ahead would leave the next an empty list.)
static FETCHED: Mutex<Option<(Vec<Hash>, std::collections::HashSet<Hash>)>> = Mutex::new(None);
const FETCHED_MAX: usize = 8192;

/// [`FETCHED`] as concatenated hashes.
pub fn fetched() -> Vec<u8> {
    FETCHED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map_or(Vec::new(), |f| f.0.concat())
}

fn note_read(h: &Hash) {
    let mut g = FETCHED.lock().unwrap_or_else(|e| e.into_inner());
    let (list, seen) = g.get_or_insert_with(Default::default);
    if list.len() < FETCHED_MAX && seen.insert(*h) {
        list.push(*h);
    }
}

// ---------------------------------------------------------------------------------------------
// fetching ahead: a read of several chunks (a CG or a voice read whole) used to fetch them one
// synchronous request after another on the game's thread; the ones after the first now go to
// a few fetcher threads at once, and so does the chunk after the read.

const FETCHERS: usize = 3;

struct Ahead {
    queue: VecDeque<Hash>,
    /// queued or being fetched
    busy: std::collections::HashSet<Hash>,
    started: bool,
}

static AHEAD: Mutex<Option<Ahead>> = Mutex::new(None);
static AHEAD_WORK: std::sync::Condvar = std::sync::Condvar::new();
static AHEAD_DONE: std::sync::Condvar = std::sync::Condvar::new();

fn cached(h: &Hash) -> bool {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|c| c.map.contains_key(h))
}

/// Has chunk `h` fetched by a fetcher thread unless it is here already.
fn fetch_ahead(h: &Hash) {
    if cached(h) {
        return;
    }
    let mut g = AHEAD.lock().unwrap_or_else(|e| e.into_inner());
    let a = g.get_or_insert_with(|| Ahead {
        queue: VecDeque::new(),
        busy: Default::default(),
        started: false,
    });
    if !a.busy.insert(*h) {
        return;
    }
    a.queue.push_back(*h);
    if !a.started {
        a.started = true;
        for i in 0..FETCHERS {
            let _ = std::thread::Builder::new()
                .name(format!("vfs fetch {i}"))
                .spawn(fetcher);
        }
    }
    AHEAD_WORK.notify_one();
}

fn fetcher() {
    loop {
        let h = {
            let mut g = AHEAD.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(h) = g.as_mut().and_then(|a| a.queue.pop_front()) {
                    break h;
                }
                g = AHEAD_WORK.wait(g).unwrap_or_else(|e| e.into_inner());
            }
        };
        if let Some(data) = fetch_chunk(&h) {
            cache_put(&h, Arc::new(data));
        }
        if let Some(a) = AHEAD.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            a.busy.remove(&h);
        }
        AHEAD_DONE.notify_all();
    }
}

/// Waits while a fetcher has chunk `h` (queued or on its way).
fn wait_ahead(h: &Hash) {
    let mut g = AHEAD.lock().unwrap_or_else(|e| e.into_inner());
    while g.as_ref().is_some_and(|a| a.busy.contains(h)) {
        g = AHEAD_DONE.wait(g).unwrap_or_else(|e| e.into_inner());
    }
}

fn fetch_chunk(h: &Hash) -> Option<Vec<u8>> {
    let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
    fetch(&format!("chunks/{}/{hex}", &hex[..2]))
}

fn chunk_data(h: &Hash, len: usize) -> Result<Arc<Vec<u8>>, c_int> {
    let t = std::time::Instant::now();
    let r = chunk_data_wait(h, len);
    if r.is_ok() {
        note_read(h);
    }
    // (a chunk in the cache returns in a few us: only real waits count)
    if t.elapsed().as_micros() > 200 {
        net::stall(t);
    }
    r
}

fn chunk_data_wait(h: &Hash, len: usize) -> Result<Arc<Vec<u8>>, c_int> {
    wait_ahead(h);
    if let Some(d) = CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|c| c.get(h, len, cache_limit()))
    {
        return Ok(d);
    }
    let mut data = fetch_chunk(h).ok_or(libc::EIO)?;
    data.resize(len, 0);
    Ok(cache_put(h, Arc::new(data)))
}

/// A chunk the page fetched ahead: `hash` (16 bytes) and the `data` it malloc'ed, which this
/// takes (the system allocator is malloc).
#[no_mangle]
pub unsafe extern "C" fn vnhost_vfs_put_chunk(hash: *const u8, data: *mut u8, len: size_t) {
    let h: Hash = std::slice::from_raw_parts(hash, 16).try_into().unwrap();
    cache_put(&h, Arc::new(Vec::from_raw_parts(data, len, len)));
}

fn cache_put(h: &Hash, data: Arc<Vec<u8>>) -> Arc<Vec<u8>> {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Cache::default)
        .put(h, data.clone(), cache_limit());
    data
}

/// Reads file content at `off` (the lock is not held while chunks are fetched).
fn read_at(ino: Ino, buf: &mut [u8], off: u64) -> Result<usize, c_int> {
    let lower = with(|fs| match fs.node(ino).map(|n| &n.data) {
        Some(Data::File(Content::Upper(v))) => {
            let start = (off as usize).min(v.len());
            let n = buf.len().min(v.len() - start);
            buf[..n].copy_from_slice(&v[start..start + n]);
            Ok(Err(n))
        }
        Some(Data::File(Content::Lower {
            size,
            chunk,
            chunks,
        })) => Ok(Ok((*size, *chunk as u64, chunks.clone()))),
        Some(Data::Dir(_)) => Err(libc::EISDIR),
        _ => Err(libc::EBADF),
    })?;
    let (size, chunk, chunks) = match lower {
        Err(n) => return Ok(n),
        Ok(l) => l,
    };
    // (a read of nothing: else end - 1 below wraps, and every chunk of the file is fetched ahead)
    if off >= size || buf.is_empty() {
        return Ok(0);
    }
    let end = (off + buf.len() as u64).min(size);
    // the read's other chunks and the one after it: fetched at once by the fetcher threads while
    // this one fetches the first
    let (first, last) = ((off / chunk) as usize, ((end - 1) / chunk) as usize + 1);
    for i in first + 1..=last.min(chunks.len().saturating_sub(1)) {
        if let Some(h) = chunks.get(i).copied().flatten() {
            fetch_ahead(&h);
        }
    }
    let mut at = off;
    while at < end {
        let i = (at / chunk) as usize;
        let cstart = i as u64 * chunk;
        let clen = chunk.min(size - cstart) as usize;
        let from = (at - cstart) as usize;
        let n = ((end - at) as usize).min(clen - from);
        let dst = &mut buf[(at - off) as usize..(at - off) as usize + n];
        match chunks.get(i).copied().flatten() {
            None => dst.fill(0),
            Some(h) => dst.copy_from_slice(&chunk_data(&h, clen)?[from..from + n]),
        }
        at += n as u64;
    }
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
        eprintln!(
            "kura: a write copies {} MiB of a file from the store into memory (inode {ino})",
            size >> 20
        );
    }
    let mut v = vec![0; size as usize];
    read_at(ino, &mut v, 0)?;
    with(|fs| {
        if let Some(Data::File(c @ Content::Lower { .. })) = fs.data_mut(ino) {
            *c = Content::Upper(v);
        }
    });
    Ok(())
}

fn write_at(ino: Ino, data: &[u8], off: u64) -> Result<usize, c_int> {
    copy_up(ino, u64::MAX)?;
    with(|fs| {
        let Some(Data::File(Content::Upper(v))) = fs.data_mut(ino) else {
            return Err(libc::EBADF);
        };
        let end = off as usize + data.len();
        if v.len() < end {
            v.resize(end, 0);
        }
        v[off as usize..end].copy_from_slice(data);
        if let Some(n) = fs.node_mut(ino) {
            n.mtime = now();
        }
        Ok(data.len())
    })
}

fn set_size(ino: Ino, size: u64) -> Result<(), c_int> {
    copy_up(ino, size)?;
    with(|fs| match fs.data_mut(ino) {
        Some(Data::File(Content::Upper(v))) => {
            v.resize(size as usize, 0);
            Ok(())
        }
        Some(Data::Dir(_)) => Err(libc::EISDIR),
        _ => Err(libc::EINVAL),
    })
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
}

impl Fs {
    fn info(&self, ino: Ino) -> Info {
        let n = self.node(ino).unwrap();
        Info {
            dir: matches!(n.data, Data::Dir(_)),
            len: self.file_size(ino),
            mtime: n.mtime,
            mode: n.mode,
        }
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
        Data::Dir(ch) => Ok(ch.keys().find(|k| k.to_lowercase() == lc).cloned()),
        _ => Err(libc::ENOTDIR),
    })
}

/// Opens `p` as a handle (released with [`release`]).
pub(crate) fn open_handle(p: &str, flags: c_int, mode: u32) -> At<u32> {
    let follow = flags & libc::O_NOFOLLOW == 0;
    let found = with(|fs| fs.resolve(p, follow));
    let ino = match found {
        Err(e) => return At::Ours(Err(e)),
        Ok(Found::Outside(p)) => return At::Outside(p),
        Ok(Found::Node { ino, .. }) => {
            if flags & libc::O_CREAT != 0 && flags & libc::O_EXCL != 0 {
                return At::Ours(Err(libc::EEXIST));
            }
            ino
        }
        Ok(Found::Missing { parent, name, .. }) => {
            if flags & libc::O_CREAT == 0 {
                return At::Ours(Err(libc::ENOENT));
            }
            with(|fs| {
                let ino = fs.add(Data::File(Content::Upper(Vec::new())), mode & 0o7777, now());
                if let Some(Data::Dir(ch)) = fs.data_mut(parent) {
                    ch.insert(name, ino);
                }
                ino
            })
        }
    };
    let kind = with(|fs| {
        fs.node(ino).map(|n| match n.data {
            Data::Dir(_) => 0,
            Data::File(_) => 1,
            Data::Link(_) => 2,
        })
    });
    match kind {
        Some(0) if flags & libc::O_ACCMODE != libc::O_RDONLY => return At::Ours(Err(libc::EISDIR)),
        Some(2) => return At::Ours(Err(libc::ELOOP)),
        None => return At::Ours(Err(libc::ENOENT)),
        _ => {}
    }
    if kind == Some(1) && flags & libc::O_TRUNC != 0 && flags & libc::O_ACCMODE != libc::O_RDONLY {
        if let Err(e) = set_size(ino, 0) {
            return At::Ours(Err(e));
        }
    }
    At::Ours(Ok(with(|fs| fs.new_handle(ino, flags))))
}

/// Closes a handle.
pub(crate) fn release(h: u32) {
    with(|fs| {
        if let Some(handle) = fs.handles.remove(&h) {
            fs.gc(handle.ino);
        }
    })
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
    let Some((parent_path, name)) = p.rsplit_once('/') else {
        return At::Ours(Err(libc::EINVAL));
    };
    let parent_path = if parent_path.is_empty() {
        "/"
    } else {
        parent_path
    };
    with(|fs| {
        let parent = match fs.resolve(parent_path, true) {
            Err(e) => return At::Ours(Err(e)),
            Ok(Found::Outside(_)) => return At::Outside(p.to_string()),
            Ok(Found::Missing { .. }) => return At::Ours(Err(libc::ENOENT)),
            Ok(Found::Node { ino, .. }) => ino,
        };
        let Some(Data::Dir(ch)) = fs.node(parent).map(|n| &n.data) else {
            return At::Ours(Err(libc::ENOTDIR));
        };
        let Some(&ino) = ch.get(name) else {
            return At::Ours(Err(libc::ENOENT));
        };
        match (&fs.node(ino).unwrap().data, dir) {
            (Data::Dir(c), true) => {
                if !c.is_empty() {
                    return At::Ours(Err(libc::ENOTEMPTY));
                }
                if let Some(n) = fs.node_mut(ino) {
                    n.nlink = 0;
                }
                if let Some(n) = fs.node_mut(parent) {
                    n.nlink -= 1;
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
        let (dp, dn) = match dst {
            Err(e) => return At::Ours(Err(e)),
            Ok(Found::Outside(_)) => return At::Ours(Err(libc::EXDEV)),
            Ok(Found::Missing { parent, name, .. }) => (parent, name),
            Ok(Found::Node { ino, path }) => {
                if ino == src_ino {
                    return At::Ours(Ok(()));
                }
                if let Some(Data::Dir(c)) = fs.node(ino).map(|n| &n.data) {
                    if !c.is_empty() {
                        return At::Ours(Err(libc::ENOTEMPTY));
                    }
                }
                let (d, n) = path.rsplit_once('/').unwrap();
                let Ok(Found::Node { ino: parent, .. }) =
                    fs.resolve(if d.is_empty() { "/" } else { d }, true)
                else {
                    return At::Ours(Err(libc::EXDEV));
                };
                fs.unlink_name(parent, n);
                (parent, n.to_string())
            }
        };
        if let Some(Data::Dir(ch)) = fs.data_mut(sp) {
            ch.remove(&sn);
        }
        if let Some(Data::Dir(ch)) = fs.data_mut(dp) {
            ch.insert(dn, src_ino);
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
    let (ino, pos, flags) = handle(h).ok_or(libc::EBADF)?;
    if flags & libc::O_ACCMODE == libc::O_WRONLY {
        return Err(libc::EBADF);
    }
    if trace() {
        eprintln!("vfs: read {h} ino {ino} {}@{pos}", buf.len());
    }
    let n = read_at(ino, buf, pos)?;
    set_pos(h, pos + n as u64);
    Ok(n)
}

pub(crate) fn h_write(h: u32, data: &[u8]) -> Result<usize, c_int> {
    let (ino, pos, flags) = handle(h).ok_or(libc::EBADF)?;
    if flags & libc::O_ACCMODE == libc::O_RDONLY {
        return Err(libc::EBADF);
    }
    let at = if flags & libc::O_APPEND != 0 {
        with(|fs| fs.file_size(ino))
    } else {
        pos
    };
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
mod tests {
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
        for _ in 0..300 {
            let limit = 1 + rnd(4000) as usize;
            let keys = 1 + rnd(12);
            let mut c = Cache::default();
            let mut model: Vec<(Hash, usize)> = Vec::new();
            for _ in 0..200 {
                let mut h = [0u8; 16];
                h[0] = rnd(keys) as u8;
                let len = rnd(1200) as usize;
                if rnd(3) == 0 {
                    let got = c.get(&h, len, limit);
                    let i = model.iter().position(|m| m.0 == h);
                    assert_eq!(got.is_some(), i.is_some());
                    if let (Some(d), Some(i)) = (got, i) {
                        assert_eq!(d.len(), len);
                        model[i].1 = len;
                    }
                } else {
                    c.put(&h, Arc::new(vec![h[0]; len]), limit);
                    match model.iter().position(|m| m.0 == h) {
                        Some(i) => model[i].1 = len,
                        None => model.push((h, len)),
                    }
                }
                while model.iter().map(|m| m.1).sum::<usize>() > limit && !model.is_empty() {
                    model.remove(0);
                }
                let sum: usize = c.map.values().map(|d| d.len()).sum();
                assert_eq!(c.bytes, sum);
                assert!(c.bytes <= limit);
                assert_eq!(c.order.len(), c.map.len());
                assert_eq!(
                    c.order.iter().cloned().collect::<Vec<_>>(),
                    model.iter().map(|m| m.0).collect::<Vec<_>>()
                );
                for (h, len) in &model {
                    assert_eq!(c.map[h].len(), *len);
                }
            }
        }
    }

    /// The store, for these tests: chunks by name, and the names fetched.
    static STORE: Mutex<Option<(HashMap<String, Vec<u8>>, Vec<String>)>> = Mutex::new(None);

    #[no_mangle]
    extern "C" fn vnvm_fetch(name: *const c_char, len: *mut size_t) -> *mut u8 {
        let name = unsafe { std::ffi::CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned();
        let mut g = STORE.lock().unwrap();
        let (m, log) = g.get_or_insert_with(Default::default);
        log.push(name.clone());
        let Some(d) = m.get(&name) else {
            return std::ptr::null_mut();
        };
        unsafe {
            let p = libc::malloc(d.len().max(1)) as *mut u8;
            std::ptr::copy_nonoverlapping(d.as_ptr(), p, d.len());
            *len = d.len();
            p
        }
    }

    /// Store files (chunks of 64 bytes, some all zero, the last short), then random writes,
    /// truncations and reads against a model of each file's bytes. A truncation to 0 of a file
    /// still in the store fetches nothing; one to n only the chunks before n (and the next).
    #[test]
    fn store_files_written_and_cut() {
        let mut seed = 0x510e527fade682d1u64;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut fs = Fs {
            next_handle: 1,
            ..Default::default()
        };
        let root = fs.add(Data::Dir(BTreeMap::new()), 0o755, 0);
        let mut tsv = String::from("vnvm-tree\t1\t64\n");
        let mut models = Vec::new();
        let mut chunks = HashMap::new();
        for f in 0..12 {
            let size = rnd(64 * 9) + 1;
            let data: Vec<u8> = (0..size)
                .map(|i| {
                    if (i / 64) % 4 == 2 {
                        0
                    } else {
                        1 + rnd(255) as u8
                    }
                })
                .collect();
            let mut names = Vec::new();
            for c in data.chunks(64) {
                if c.iter().all(|&b| b == 0) {
                    names.push(String::new());
                    continue;
                }
                let h: String = (0..16).map(|_| format!("{:02x}", rnd(256))).collect();
                // (stored chunks may end short: their zero tail is cut)
                let mut stored = c.to_vec();
                while stored.last() == Some(&0) {
                    stored.pop();
                }
                chunks.insert(format!("chunks/{}/{h}", &h[..2]), stored);
                names.push(h);
            }
            tsv += &format!("f\t644\t0\tf{f}\t{size}\t{}\n", names.join(","));
            models.push(data);
        }
        fs.load_tree(root, &tsv);
        let inos: Vec<Ino> = (0..12)
            .map(|f| match fs.node(root).map(|n| &n.data) {
                Some(Data::Dir(c)) => c[&format!("f{f}")],
                _ => unreachable!(),
            })
            .collect();
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
            let n = if f % 3 == 0 {
                0
            } else {
                rnd(models[f].len() as u64 + 100)
            };
            fetched();
            set_size(ino, n).unwrap();
            let got = fetched();
            // (and the chunk after them, fetched ahead as after any read)
            let need = (n.min(models[f].len() as u64)).div_ceil(64) as usize;
            let need = if need == 0 { 0 } else { need + 1 };
            assert!(
                got.len() <= need,
                "f{f} cut to {n}: fetched {} chunks, needs {need}",
                got.len()
            );
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
                    let want =
                        &m[(off as usize).min(m.len())..(off as usize + buf.len()).min(m.len())];
                    assert_eq!(&buf[..n], want);
                }
            }
        }
    }

    /// A padded get copies only a chunk someone else holds.
    #[test]
    fn cache_pads_in_place() {
        let mut c = Cache::default();
        let h = [1u8; 16];
        c.put(&h, Arc::new(vec![7; 10]), 1000);
        let p = Arc::as_ptr(&c.map[&h]);
        let d = c.get(&h, 20, 1000).unwrap();
        assert_eq!(Arc::as_ptr(&d), p);
        assert_eq!(
            (&d[..10], &d[10..], c.bytes),
            (&[7u8; 10][..], &[0u8; 10][..], 20)
        );
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

    /// `ippai pack`'s escaping for the tree list (crates/ippai/src/pack.rs).
    fn esc(s: &str) -> String {
        s.replace('\\', "\\\\")
            .replace('\t', "\\t")
            .replace('\n', "\\n")
    }

    fn mounted() -> (Fs, Ino) {
        let mut fs = Fs {
            next_handle: 1,
            ..Default::default()
        };
        let root = fs.add(Data::Dir(BTreeMap::new()), 0o755, 0);
        fs.mounts.push(("/".into(), root));
        (fs, root)
    }

    /// A saved file's place: up to two directory levels, a file name (never one of the
    /// directory names, so no path is both).
    fn save_path() -> impl Strategy<Value = String> {
        (
            vec(
                prop::sample::select(vec!["a", "b", "セーブ", "dir with space"]),
                0..3,
            ),
            prop::sample::select(vec!["x.sav", "y.dat", "global.sav", "名前.bin"]),
        )
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
            let mut tsv = String::from("vnvm-tree\t1\t262144\n");
            let mut want = BTreeMap::new();
            for (i, n) in names.iter().enumerate() {
                let n = format!("{i}{n}");
                tsv += &format!("f\t{mode:o}\t{mtime}\t{}\t{}\t\n", esc(&format!("d/{n}")), i * 7);
                want.insert(n, i as u64 * 7);
            }
            tsv = format!("vnvm-tree\t1\t262144\nd\t755\t0\td\n{}", &tsv["vnvm-tree\t1\t262144\n".len()..]);
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
}
