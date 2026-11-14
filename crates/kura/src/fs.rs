//! Files for Rust callers (sake), shaped like the parts of std::fs they use: the trees mounted
//! here (vfs, the page's whole file system) directly, and on node whatever is outside them
//! through std::fs. Paths are absolute host paths.

use std::ffi::{c_int, OsString};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::vfs::{self, At, Info};

fn os(e: c_int) -> io::Error {
    io::Error::from_raw_os_error(e)
}

fn path_str(p: &Path) -> String {
    vfs::clean(&p.to_string_lossy())
}

#[derive(Clone, Debug)]
pub struct Metadata {
    dir: bool,
    len: u64,
    modified: SystemTime,
    readonly: bool,
    /// the file's inode number (0: unknown): the same file under any name, kept by renames
    ino: u64,
    /// its names (hard links; 1 for a directory, as Windows counts)
    links: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Permissions(bool);

impl Permissions {
    pub fn readonly(&self) -> bool {
        self.0
    }
}

impl Metadata {
    pub fn is_dir(&self) -> bool {
        self.dir
    }
    pub fn is_file(&self) -> bool {
        !self.dir
    }
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn modified(&self) -> io::Result<SystemTime> {
        Ok(self.modified)
    }
    pub fn permissions(&self) -> Permissions {
        Permissions(self.readonly)
    }
    pub fn ino(&self) -> u64 {
        self.ino
    }
    pub fn links(&self) -> u32 {
        self.links
    }
}

impl From<Info> for Metadata {
    fn from(i: Info) -> Metadata {
        Metadata {
            dir: i.dir,
            len: if i.dir { 0 } else { i.len },
            modified: UNIX_EPOCH + Duration::from_secs(i.mtime.max(0) as u64),
            readonly: i.mode & 0o200 == 0,
            ino: i.ino as u64 | 1 << 40,
            links: 1,
        }
    }
}

impl From<std::fs::Metadata> for Metadata {
    fn from(m: std::fs::Metadata) -> Metadata {
        #[cfg(unix)]
        let ino = std::os::unix::fs::MetadataExt::ino(&m);
        #[cfg(not(unix))]
        let ino = 0;
        #[cfg(unix)]
        let links = if m.is_dir() { 1 } else { std::os::unix::fs::MetadataExt::nlink(&m).clamp(1, u32::MAX as u64) as u32 };
        #[cfg(not(unix))]
        let links = 1;
        Metadata { dir: m.is_dir(), len: m.len(), modified: m.modified().unwrap_or(UNIX_EPOCH), readonly: m.permissions().readonly(), ino, links }
    }
}

pub fn metadata<P: AsRef<Path>>(p: P) -> io::Result<Metadata> {
    match vfs::info_at(&path_str(p.as_ref()), true) {
        At::Ours(r) => r.map(Metadata::from).map_err(os),
        At::Outside(h) => std::fs::metadata(h).map(Metadata::from),
    }
}

pub fn symlink_metadata<P: AsRef<Path>>(p: P) -> io::Result<Metadata> {
    match vfs::info_at(&path_str(p.as_ref()), false) {
        At::Ours(r) => r.map(Metadata::from).map_err(os),
        At::Outside(h) => std::fs::symlink_metadata(h).map(Metadata::from),
    }
}

/// A number that changes whenever the file at `p` may have changed (what it holds, being made,
/// removed or renamed over): what a cache of a file's contents checks before it trusts them.
/// In the trees here it is the file system's change count (any change anywhere changes it);
/// outside them (node's own files) the file's size, time and inode, and 0 (trust nothing) for
/// a file that is not there or was changed in the last `RECENT` (a host's file times move in
/// ticks of a few ms: a rewrite of the same size within one would look the same). Take it
/// before reading the file: a change while reading then shows at the next look.
pub fn stamp<P: AsRef<Path>>(p: P) -> u64 {
    let count = vfs::change_count();
    match vfs::info_at(&path_str(p.as_ref()), true) {
        At::Ours(_) => 1 << 63 | count as u64,
        At::Outside(h) => match std::fs::metadata(h) {
            Err(_) => 0,
            Ok(m) if m.modified().ok().and_then(|t| SystemTime::now().duration_since(t).ok()).is_none_or(|d| d < RECENT) => 0,
            Ok(m) => {
                use std::hash::{Hash, Hasher};
                let mut s = std::collections::hash_map::DefaultHasher::new();
                m.len().hash(&mut s);
                m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos()).hash(&mut s);
                Metadata::from(m).ino().hash(&mut s);
                s.finish() >> 1 | 1
            }
        },
    }
}

/// How long a host file's change keeps its stamp 0 (`stamp`)
pub const RECENT: Duration = Duration::from_secs(2);

pub fn read<P: AsRef<Path>>(p: P) -> io::Result<Vec<u8>> {
    let mut f = File::open(p)?;
    let mut v = Vec::with_capacity(f.metadata().map_or(0, |m| m.len() as usize));
    f.read_to_end(&mut v)?;
    Ok(v)
}

pub fn write<P: AsRef<Path>, C: AsRef<[u8]>>(p: P, data: C) -> io::Result<()> {
    File::create(p)?.write_all(data.as_ref())
}

/// The first N bytes of P (fewer when it is shorter): headers, without reading a 200 MB file
/// whole (#234).
pub fn read_head<P: AsRef<Path>>(p: P, n: usize) -> io::Result<Vec<u8>> {
    let mut v = Vec::new();
    File::open(p)?.take(n as u64).read_to_end(&mut v)?;
    Ok(v)
}

/// Copied through a 1 MiB buffer: an installer copies its 200 MB payload (SHFileOperation), and
/// a whole-file read held it all in the page's memory twice (#234).
pub fn copy<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<u64> {
    let mut src = File::open(&from)?;
    if from.as_ref() == to.as_ref() {
        // (the read-then-write copy kept such a file; truncating it first would empty it)
        return Ok(src.metadata()?.len());
    }
    let mut dst = File::create(to)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let got = match src.read(&mut buf) {
            Ok(0) => return Ok(n),
            Ok(k) => k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        dst.write_all(&buf[..got])?;
        n += got as u64;
    }
}

pub fn create_dir<P: AsRef<Path>>(p: P) -> io::Result<()> {
    match vfs::mkdir_at(&path_str(p.as_ref())) {
        At::Ours(r) => r.map_err(os),
        At::Outside(h) => std::fs::create_dir(h),
    }
}

pub fn create_dir_all<P: AsRef<Path>>(p: P) -> io::Result<()> {
    let p = path_str(p.as_ref());
    let mut at = String::new();
    for c in p.split('/').filter(|c| !c.is_empty()) {
        at.push('/');
        at.push_str(c);
        match create_dir(&at) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if !metadata(&at)?.is_dir() {
                    return Err(e);
                }
            }
            r => r?,
        }
    }
    Ok(())
}

pub fn remove_file<P: AsRef<Path>>(p: P) -> io::Result<()> {
    match vfs::remove_at(&path_str(p.as_ref()), false) {
        At::Ours(r) => r.map_err(os),
        At::Outside(h) => std::fs::remove_file(h),
    }
}

pub fn remove_dir<P: AsRef<Path>>(p: P) -> io::Result<()> {
    match vfs::remove_at(&path_str(p.as_ref()), true) {
        At::Ours(r) => r.map_err(os),
        At::Outside(h) => std::fs::remove_dir(h),
    }
}

/// A second name `to` of file `from` (std::fs::hard_link) outside the trees mounted here; in
/// them, or across: Unsupported (they keep one name per file).
pub fn hard_link<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<()> {
    let (a, b) = (path_str(from.as_ref()), path_str(to.as_ref()));
    match (vfs::info_at(&a, true), vfs::info_at(&b, true)) {
        (At::Outside(ha), At::Outside(hb)) => host_link(Path::new(&ha), Path::new(&hb)),
        _ => Err(io::ErrorKind::Unsupported.into()),
    }
}

#[cfg(not(target_os = "emscripten"))]
fn host_link(a: &Path, b: &Path) -> io::Result<()> {
    std::fs::hard_link(a, b)
}
/// (node's own fs: NODERAWFS has no link; the page has no host files: Unsupported)
#[cfg(target_os = "emscripten")]
fn host_link(a: &Path, b: &Path) -> io::Result<()> {
    extern "C" {
        fn vnhost_hard_link(a: *const std::ffi::c_char, b: *const std::ffi::c_char) -> i32;
    }
    let c = |p: &Path| std::ffi::CString::new(p.to_string_lossy().into_owned()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput));
    let (ca, cb) = (c(a)?, c(b)?);
    match unsafe { vnhost_hard_link(ca.as_ptr(), cb.as_ptr()) } {
        0 => Ok(()),
        -1 => Err(io::ErrorKind::Unsupported.into()),
        -2 => Err(io::ErrorKind::AlreadyExists.into()),
        -3 => Err(io::ErrorKind::NotFound.into()),
        _ => Err(io::ErrorKind::PermissionDenied.into()),
    }
}

pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<()> {
    let (a, b) = (path_str(from.as_ref()), path_str(to.as_ref()));
    match vfs::rename_at(&a, &b) {
        At::Ours(r) => r.map_err(os),
        At::Outside(_) => std::fs::rename(a, b),
    }
}

/// The real name of `name` in directory `dir` when only its case differs (no listing of the
/// directory as entries).
pub fn name_nocase(dir: &str, name: &str) -> Option<String> {
    match vfs::name_nocase(&vfs::clean(dir), name) {
        At::Ours(r) => r.ok().flatten(),
        At::Outside(h) => {
            let lc = name.to_lowercase();
            std::fs::read_dir(h).ok()?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).find(|n| n.to_lowercase() == lc)
        }
    }
}

pub struct DirEntry {
    name: String,
    path: PathBuf,
    meta: Option<Metadata>,
}

impl DirEntry {
    pub fn file_name(&self) -> OsString {
        OsString::from(&self.name)
    }
    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        match &self.meta {
            Some(m) => Ok(m.clone()),
            None => metadata(&self.path),
        }
    }
}

pub struct ReadDir(std::vec::IntoIter<io::Result<DirEntry>>);

impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
}

/// The entries of a directory (without "." and "..").
pub fn read_dir<P: AsRef<Path>>(p: P) -> io::Result<ReadDir> {
    let dir = path_str(p.as_ref());
    let v: Vec<io::Result<DirEntry>> = match vfs::list_at(&dir) {
        At::Ours(r) => r
            .map_err(os)?
            .into_iter()
            .map(|(name, i)| Ok(DirEntry { path: Path::new(&dir).join(&name), name, meta: Some(i.into()) }))
            .collect(),
        At::Outside(h) => std::fs::read_dir(h)?
            .map(|e| e.map(|e| DirEntry { name: e.file_name().to_string_lossy().into_owned(), path: e.path(), meta: None }))
            .collect(),
    };
    Ok(ReadDir(v.into_iter()))
}

#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
}

impl OpenOptions {
    pub fn new() -> OpenOptions {
        OpenOptions::default()
    }
    pub fn read(&mut self, v: bool) -> &mut Self {
        self.read = v;
        self
    }
    pub fn write(&mut self, v: bool) -> &mut Self {
        self.write = v;
        self
    }
    pub fn append(&mut self, v: bool) -> &mut Self {
        self.append = v;
        self
    }
    pub fn truncate(&mut self, v: bool) -> &mut Self {
        self.truncate = v;
        self
    }
    pub fn create(&mut self, v: bool) -> &mut Self {
        self.create = v;
        self
    }
    pub fn create_new(&mut self, v: bool) -> &mut Self {
        self.create_new = v;
        self
    }
    pub fn open<P: AsRef<Path>>(&self, p: P) -> io::Result<File> {
        let write = self.write || self.append;
        let mut flags = match (self.read, write) {
            (_, false) => libc::O_RDONLY,
            (false, true) => libc::O_WRONLY,
            (true, true) => libc::O_RDWR,
        };
        if self.append {
            flags |= libc::O_APPEND;
        }
        if self.truncate {
            flags |= libc::O_TRUNC;
        }
        if self.create || self.create_new {
            flags |= libc::O_CREAT;
        }
        if self.create_new {
            flags |= libc::O_EXCL;
        }
        match vfs::open_handle(&path_str(p.as_ref()), flags, 0o644) {
            At::Ours(r) => r.map(|h| File(Inner::Vfs(h))).map_err(os),
            At::Outside(h) => std::fs::OpenOptions::new()
                .read(self.read)
                .write(self.write)
                .append(self.append)
                .truncate(self.truncate)
                .create(self.create)
                .create_new(self.create_new)
                .open(h)
                .map(|f| File(Inner::Std(f))),
        }
    }
}

enum Inner {
    Vfs(u32),
    Std(std::fs::File),
}

pub struct File(Inner);

impl File {
    pub fn open<P: AsRef<Path>>(p: P) -> io::Result<File> {
        OpenOptions::new().read(true).open(p)
    }
    pub fn create<P: AsRef<Path>>(p: P) -> io::Result<File> {
        OpenOptions::new().write(true).create(true).truncate(true).open(p)
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        match &self.0 {
            Inner::Vfs(h) => vfs::h_info(*h).map(Metadata::from).ok_or_else(|| os(libc::EBADF)),
            Inner::Std(f) => f.metadata().map(Metadata::from),
        }
    }
    /// Starts fetching bytes `off..off + len` from the store (VFS files; nothing else), without
    /// waiting: a later read of them finds them here.
    pub fn prefetch(&self, off: u64, len: u64) {
        if let Inner::Vfs(h) = &self.0 {
            vfs::h_prefetch(*h, off, len);
        }
    }
    pub fn set_len(&self, size: u64) -> io::Result<()> {
        match &self.0 {
            Inner::Vfs(h) => vfs::h_set_len(*h, size).map_err(os),
            Inner::Std(f) => f.set_len(size),
        }
    }
}

impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.0 {
            Inner::Vfs(h) => vfs::h_read(*h, buf).map_err(os),
            Inner::Std(f) => f.read(buf),
        }
    }
}

impl Write for File {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match &mut self.0 {
            Inner::Vfs(h) => vfs::h_write(*h, data).map_err(os),
            Inner::Std(f) => f.write(data),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match &mut self.0 {
            Inner::Vfs(_) => Ok(()),
            Inner::Std(f) => f.flush(),
        }
    }
}

impl Seek for File {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match &mut self.0 {
            Inner::Vfs(h) => {
                let (off, whence) = match pos {
                    SeekFrom::Start(o) => (o as i64, libc::SEEK_SET),
                    SeekFrom::Current(o) => (o, libc::SEEK_CUR),
                    SeekFrom::End(o) => (o, libc::SEEK_END),
                };
                vfs::h_seek(*h, off, whence).map_err(os)
            }
            Inner::Std(f) => f.seek(pos),
        }
    }
}

impl Drop for File {
    fn drop(&mut self) {
        if let Inner::Vfs(h) = self.0 {
            vfs::release(h);
        }
    }
}

#[cfg(test)]
mod link_tests {
    use super::*;

    /// A host file's second name: one inode, both names count 2 links, writes seen through
    /// either, the second kept when the first goes; an existing new name and a missing file
    /// fail with their kinds
    #[test]
    fn hard_links_on_host_files() {
        let d = std::env::temp_dir().join(format!("kura-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (a, b) = (d.join("a"), d.join("b"));
        write(&a, b"one").unwrap();
        assert_eq!(metadata(&a).unwrap().links(), 1);
        hard_link(&a, &b).unwrap();
        let (ma, mb) = (metadata(&a).unwrap(), metadata(&b).unwrap());
        assert_eq!((ma.links(), mb.links()), (2, 2));
        assert_eq!(ma.ino(), mb.ino());
        std::fs::OpenOptions::new().append(true).open(&b).unwrap().write_all(b"+b").unwrap();
        assert_eq!(read(&a).unwrap(), b"one+b");
        assert_eq!(hard_link(&a, &b).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(hard_link(d.join("none"), d.join("c")).unwrap_err().kind(), io::ErrorKind::NotFound);
        remove_file(&a).unwrap();
        assert_eq!(metadata(&b).unwrap().links(), 1);
        assert_eq!(read(&b).unwrap(), b"one+b");
        assert_eq!(metadata(&d).unwrap().links(), 1);
        std::fs::remove_dir_all(&d).unwrap();
    }
}

#[cfg(test)]
mod stamp_tests {
    use super::*;

    fn age(p: &Path, secs: u64) {
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(secs)).unwrap();
    }

    /// A host file's stamp: 0 when missing or just changed (a same-size rewrite in the same
    /// tick must not look unchanged), the same while nothing changes, new for a rewrite of the
    /// same size at another time, a rename over it and a removal
    #[test]
    fn host_file_stamps() {
        let d = std::env::temp_dir().join(format!("kura-stamp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let a = d.join("a.ini");
        assert_eq!(stamp(&a), 0);
        write(&a, b"k=1").unwrap();
        assert_eq!(stamp(&a), 0, "just written");
        age(&a, 60);
        let s1 = stamp(&a);
        assert_ne!(s1, 0);
        assert_eq!(stamp(&a), s1);
        let _ = read(&a).unwrap();
        assert_eq!(stamp(&a), s1, "reading changes nothing");
        write(&a, b"k=2").unwrap();
        assert_eq!(stamp(&a), 0, "same size, just written");
        age(&a, 50);
        let s2 = stamp(&a);
        assert!(s2 != 0 && s2 != s1);
        let b = d.join("b.ini");
        write(&b, b"k=2").unwrap();
        age(&b, 50);
        rename(&b, &a).unwrap();
        let s3 = stamp(&a);
        assert!(s3 != 0 && s3 != s2, "another inode");
        remove_file(&a).unwrap();
        assert_eq!(stamp(&a), 0);
        std::fs::remove_dir_all(&d).unwrap();
    }
}

#[cfg(test)]
mod head_copy_tests {
    use proptest::prelude::*;

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("kura-fs-copy-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// copy gives the same bytes whatever the size (across the 1 MiB buffer's edges), over
        /// a longer file too; read_head is the file's first N bytes.
        #[test]
        fn copy_and_head_match_the_file(len in prop_oneof![0usize..5000, (1usize << 20) - 3..(1 << 20) + 3, 0usize..3 << 20], old in 0usize..5000, n in 0usize..6000, seed in any::<u8>()) {
            let d = dir();
            let (a, b) = (d.join(format!("a{seed}-{len}")), d.join(format!("b{seed}-{len}")));
            let data: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect();
            std::fs::write(&a, &data).unwrap();
            std::fs::write(&b, vec![7u8; old]).unwrap();
            prop_assert_eq!(super::copy(&a, &b).unwrap(), len as u64);
            prop_assert!(std::fs::read(&b).unwrap() == data);
            prop_assert!(super::read_head(&a, n).unwrap() == data[..n.min(len)]);
            // onto itself: kept
            prop_assert_eq!(super::copy(&a, &a).unwrap(), len as u64);
            prop_assert!(std::fs::read(&a).unwrap() == data);
            std::fs::remove_file(&a).unwrap();
            std::fs::remove_file(&b).unwrap();
        }
    }

    #[test]
    fn copy_of_a_missing_file_fails_and_makes_nothing() {
        let d = dir();
        assert!(super::copy(d.join("none"), d.join("made")).is_err());
        assert!(!d.join("made").exists());
        assert!(super::read_head(d.join("none"), 4).is_err());
    }
}
