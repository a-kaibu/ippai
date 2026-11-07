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
}

impl From<Info> for Metadata {
    fn from(i: Info) -> Metadata {
        Metadata {
            dir: i.dir,
            len: if i.dir { 0 } else { i.len },
            modified: UNIX_EPOCH + Duration::from_secs(i.mtime.max(0) as u64),
            readonly: i.mode & 0o200 == 0,
        }
    }
}

impl From<std::fs::Metadata> for Metadata {
    fn from(m: std::fs::Metadata) -> Metadata {
        Metadata { dir: m.is_dir(), len: m.len(), modified: m.modified().unwrap_or(UNIX_EPOCH), readonly: m.permissions().readonly() }
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

pub fn read<P: AsRef<Path>>(p: P) -> io::Result<Vec<u8>> {
    let mut f = File::open(p)?;
    let mut v = Vec::with_capacity(f.metadata().map_or(0, |m| m.len() as usize));
    f.read_to_end(&mut v)?;
    Ok(v)
}

pub fn write<P: AsRef<Path>, C: AsRef<[u8]>>(p: P, data: C) -> io::Result<()> {
    File::create(p)?.write_all(data.as_ref())
}

pub fn copy<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<u64> {
    let d = read(from)?;
    write(to, &d)?;
    Ok(d.len() as u64)
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
