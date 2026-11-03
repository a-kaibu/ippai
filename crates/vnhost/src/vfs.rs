//! The page's file system: directory trees packed by `vnvm pack` (`trees/<name>.tsv` plus
//! chunks in the store) mounted where `VNVM_MOUNTS` says ("/=:/vnvm/game=reunion"), read
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
    Lower { size: u64, chunk: u32, chunks: Arc<Vec<Option<Hash>>> },
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
    f(g.get_or_insert_with(Fs::load))
}

extern "C" {
    /// host/vnfs.js: reads `name` from the store into a malloc'ed buffer (NULL if missing)
    fn vnvm_fetch(name: *const c_char, len: *mut size_t) -> *mut u8;
}

fn fetch(name: &str) -> Option<Vec<u8>> {
    let c = CString::new(name).ok()?;
    let mut len = 0;
    unsafe {
        let p = crate::wrap::load_idle(|| vnvm_fetch(c.as_ptr(), &mut len));
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
    *ON.get_or_init(|| std::env::var_os("VNVM_TRACE_VFS").is_some())
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
        let Ok(spec) = std::env::var("VNVM_MOUNTS") else { return fs };
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
                    None => eprintln!("vnhost: tree {name} not found in the store"),
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
        let chunk: u32 = lines.next().and_then(|h| h.split('\t').nth(2)?.parse().ok()).unwrap_or(1 << 18);
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
                    let chunks = if f[5].is_empty() { Vec::new() } else { f[5].split(',').map(hex16).collect() };
                    Data::File(Content::Lower { size, chunk, chunks: Arc::new(chunks) })
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
        let n = Node { nlink: if matches!(data, Data::Dir(_)) { 2 } else { 1 }, data, mode, mtime };
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
        id
    }

    fn unlink_name(&mut self, parent: Ino, name: &str) {
        let Some(Data::Dir(ch)) = self.data_mut(parent) else { return };
        let Some(ino) = ch.remove(name) else { return };
        if let Some(n) = self.node_mut(ino) {
            n.nlink = n.nlink.saturating_sub(1);
        }
        self.gc(ino);
    }

    /// Frees a node once no name or open handle refers to it.
    fn gc(&mut self, ino: Ino) {
        let linked = self.node(ino).is_some_and(|n| n.nlink > 0 && !(matches!(n.data, Data::Dir(_)) && n.nlink < 2));
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

struct Cache {
    map: HashMap<Hash, Arc<Vec<u8>>>,
    order: VecDeque<Hash>,
    bytes: usize,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

fn cache_limit() -> usize {
    static LIMIT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *LIMIT.get_or_init(|| std::env::var("VNVM_CHUNK_CACHE_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(128usize) << 20)
}

/// The chunks this run fetched, in order (the first FETCHED_MAX): the next run's page fetches
/// them ahead, several at a time (see jit::maybe_save). Here each is one synchronous request.
static FETCHED: Mutex<Vec<Hash>> = Mutex::new(Vec::new());
const FETCHED_MAX: usize = 8192;

/// [`FETCHED`] as concatenated hashes.
pub fn fetched() -> Vec<u8> {
    FETCHED.lock().unwrap_or_else(|e| e.into_inner()).concat()
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
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|c| c.map.contains_key(h))
}

/// Has chunk `h` fetched by a fetcher thread unless it is here already.
fn fetch_ahead(h: &Hash) {
    if cached(h) {
        return;
    }
    let mut g = AHEAD.lock().unwrap_or_else(|e| e.into_inner());
    let a = g.get_or_insert_with(|| Ahead { queue: VecDeque::new(), busy: Default::default(), started: false });
    if !a.busy.insert(*h) {
        return;
    }
    a.queue.push_back(*h);
    if !a.started {
        a.started = true;
        for i in 0..FETCHERS {
            let _ = std::thread::Builder::new().name(format!("vfs fetch {i}")).spawn(fetcher);
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
    let data = fetch(&format!("chunks/{}/{hex}", &hex[..2]))?;
    let mut f = FETCHED.lock().unwrap_or_else(|e| e.into_inner());
    if f.len() < FETCHED_MAX {
        f.push(*h);
    }
    Some(data)
}

fn chunk_data(h: &Hash, len: usize) -> Result<Arc<Vec<u8>>, c_int> {
    wait_ahead(h);
    {
        let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = g.as_mut().and_then(|c| c.map.get(h)) {
            if c.len() == len {
                return Ok(c.clone());
            }
            // fetched ahead, as stored: padded in place (a copy only if someone holds it)
            let c = g.as_mut().unwrap().map.remove(h).unwrap();
            let mut d = Arc::try_unwrap(c).unwrap_or_else(|c| c.to_vec());
            d.resize(len, 0);
            let d = Arc::new(d);
            g.as_mut().unwrap().map.insert(*h, d.clone());
            return Ok(d);
        }
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
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let c = g.get_or_insert_with(|| Cache { map: HashMap::new(), order: VecDeque::new(), bytes: 0 });
    if c.map.insert(*h, data.clone()).is_none() {
        c.order.push_back(*h);
        c.bytes += data.len();
        while c.bytes > cache_limit() {
            let Some(old) = c.order.pop_front() else { break };
            if let Some(d) = c.map.remove(&old) {
                c.bytes -= d.len();
            }
        }
    }
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
        Some(Data::File(Content::Lower { size, chunk, chunks })) => Ok(Ok((*size, *chunk as u64, chunks.clone()))),
        Some(Data::Dir(_)) => Err(libc::EISDIR),
        _ => Err(libc::EBADF),
    })?;
    let (size, chunk, chunks) = match lower {
        Err(n) => return Ok(n),
        Ok(l) => l,
    };
    if off >= size {
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

/// Turns a file from the store into one in memory, before it is written.
fn copy_up(ino: Ino) -> Result<(), c_int> {
    let size = with(|fs| match fs.node(ino).map(|n| &n.data) {
        Some(Data::File(Content::Lower { size, .. })) => Some(*size),
        _ => None,
    });
    let Some(size) = size else { return Ok(()) };
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
    copy_up(ino)?;
    with(|fs| {
        let Some(Data::File(Content::Upper(v))) = fs.data_mut(ino) else { return Err(libc::EBADF) };
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
    copy_up(ino)?;
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
        Info { dir: matches!(n.data, Data::Dir(_)), len: self.file_size(ino), mtime: n.mtime, mode: n.mode }
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
    let kind = with(|fs| fs.node(ino).map(|n| match n.data {
        Data::Dir(_) => 0,
        Data::File(_) => 1,
        Data::Link(_) => 2,
    }));
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
                let Ok(Found::Node { ino: parent, .. }) = fs.resolve(if d.is_empty() { "/" } else { d }, true) else {
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
