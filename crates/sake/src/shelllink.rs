//! The shell's link object (CLSID_ShellLink) as installers make Start menu and desktop
//! shortcuts with it: IShellLinkW / IShellLinkA to set and read the target, arguments, working
//! directory, description, icon, show command and hot key, and IPersistFile to save it as a
//! .lnk file (MS-SHLLINK: the header, a LinkInfo with the local path, the strings in UTF-16)
//! and load one again. One object, its interfaces at fixed offsets sharing the reference count.
//! Not done: item ID lists (GetIDList / SetIDList fail; a loaded link's ID list is skipped),
//! the extra data blocks, IPropertyStore (AppUserModelID).
#![allow(non_snake_case)]

use crate::api::Ctx;
use crate::k32::arg_str;
use crate::mem;
use crate::HashMap;
use std::sync::Mutex;

/// What a link holds
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Link {
    pub path: String,
    pub args: String,
    pub dir: String,
    pub desc: String,
    pub rel: String,
    pub icon: String,
    pub icon_index: i32,
    pub show: u32,
    pub hotkey: u16,
}

const HAS_LINK_INFO: u32 = 0x2;
const HAS_NAME: u32 = 0x4;
const HAS_RELATIVE_PATH: u32 = 0x8;
const HAS_WORKING_DIR: u32 = 0x10;
const HAS_ARGUMENTS: u32 = 0x20;
const HAS_ICON_LOCATION: u32 = 0x40;
const IS_UNICODE: u32 = 0x80;
const HAS_ID_LIST: u32 = 0x1;

/// CLSID_ShellLink {00021401-0000-0000-C000-000000000046}
const CLSID: [u8; 16] = [0x01, 0x14, 0x02, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];

/// The .lnk file of a link
pub fn encode(l: &Link) -> Vec<u8> {
    let strings = [(HAS_NAME, &l.desc), (HAS_RELATIVE_PATH, &l.rel), (HAS_WORKING_DIR, &l.dir), (HAS_ARGUMENTS, &l.args), (HAS_ICON_LOCATION, &l.icon)];
    let mut flags = IS_UNICODE;
    if !l.path.is_empty() {
        flags |= HAS_LINK_INFO;
    }
    for (f, s) in strings {
        if !s.is_empty() {
            flags |= f;
        }
    }
    let mut b = Vec::new();
    b.extend_from_slice(&0x4cu32.to_le_bytes());
    b.extend_from_slice(&CLSID);
    b.extend_from_slice(&flags.to_le_bytes());
    // file attributes, three times, size: unknown
    b.extend_from_slice(&[0u8; 4 + 24 + 4]);
    b.extend_from_slice(&l.icon_index.to_le_bytes());
    b.extend_from_slice(&(if l.show == 0 { 1 } else { l.show }).to_le_bytes());
    b.extend_from_slice(&l.hotkey.to_le_bytes());
    b.extend_from_slice(&[0u8; 10]);
    if flags & HAS_LINK_INFO != 0 {
        // LinkInfo: VolumeIDAndLocalBasePath (a fixed drive, no label), the path in the ANSI
        // code page, no common suffix
        let path = crate::text::s2a(&l.path);
        let volume: Vec<u8> = [0x11u32, 3, 0, 0x10].iter().flat_map(|v| v.to_le_bytes()).chain([0]).collect();
        let head = 0x1cu32;
        let base_at = head + volume.len() as u32;
        let suffix_at = base_at + path.len() as u32 + 1;
        let size = suffix_at + 1;
        for v in [size, head, 1, head, base_at, 0, suffix_at] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&volume);
        b.extend_from_slice(&path);
        b.extend_from_slice(&[0, 0]);
    }
    for (f, s) in strings {
        if flags & f != 0 {
            let w: Vec<u16> = s.encode_utf16().collect();
            b.extend_from_slice(&(w.len() as u16).to_le_bytes());
            b.extend(w.iter().flat_map(|u| u.to_le_bytes()));
        }
    }
    // no extra data: the terminal block
    b.extend_from_slice(&[0; 4]);
    b
}

/// A .lnk file's link (None when it is not one)
pub fn decode(b: &[u8]) -> Option<Link> {
    let r16 = |o: usize| b.get(o..o + 2).map(|x| u16::from_le_bytes([x[0], x[1]]));
    let r32 = |o: usize| b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]));
    if r32(0)? != 0x4c || b.get(4..20)? != CLSID {
        return None;
    }
    let flags = r32(20)?;
    let mut l = Link { icon_index: r32(56)? as i32, show: r32(60)?, hotkey: r16(64)?, ..Link::default() };
    let mut at = 0x4c;
    if flags & HAS_ID_LIST != 0 {
        at += 2 + r16(at)? as usize;
    }
    if flags & HAS_LINK_INFO != 0 {
        let size = r32(at)? as usize;
        let head = r32(at + 4)?;
        let info_flags = r32(at + 8)?;
        if info_flags & 1 != 0 {
            let cstr = |o: usize| -> Option<Vec<u8>> {
                let s = b.get(o..)?;
                Some(s[..s.iter().position(|&c| c == 0)?].to_vec())
            };
            let wstr = |o: usize| -> Option<String> {
                let mut v = Vec::new();
                loop {
                    let u = r16(o + 2 * v.len())?;
                    if u == 0 {
                        break;
                    }
                    v.push(u);
                }
                Some(String::from_utf16_lossy(&v))
            };
            // (a header of 0x24 bytes or more has the Unicode base path and suffix too)
            let (base, suffix) = if head >= 0x24 {
                (wstr(at + r32(at + 0x1c)? as usize)?, wstr(at + r32(at + 0x20)? as usize)?)
            } else {
                let a = |v: Vec<u8>| crate::text::a2s(&v);
                (a(cstr(at + r32(at + 16)? as usize)?), a(cstr(at + r32(at + 24)? as usize)?))
            };
            l.path = if suffix.is_empty() || base.ends_with('\\') { base + &suffix } else { format!("{base}\\{suffix}") };
        }
        at += size;
    }
    let unicode = flags & IS_UNICODE != 0;
    for (f, slot) in [(HAS_NAME, &mut l.desc), (HAS_RELATIVE_PATH, &mut l.rel), (HAS_WORKING_DIR, &mut l.dir), (HAS_ARGUMENTS, &mut l.args), (HAS_ICON_LOCATION, &mut l.icon)] {
        if flags & f == 0 {
            continue;
        }
        let n = r16(at)? as usize;
        at += 2;
        *slot = if unicode {
            let v: Option<Vec<u16>> = (0..n).map(|k| r16(at + 2 * k)).collect();
            at += 2 * n;
            String::from_utf16_lossy(&v?)
        } else {
            let s = crate::text::a2s(b.get(at..at + n)?);
            at += n;
            s
        };
    }
    Some(l)
}

struct State {
    link: Link,
    file: Option<String>,
    dirty: bool,
    refs: u32,
}

static LINKS: Mutex<Option<HashMap<u32, State>>> = Mutex::new(None);

fn links<R>(f: impl FnOnce(&mut HashMap<u32, State>) -> R) -> R {
    f(LINKS.lock().unwrap().get_or_insert_with(HashMap::default))
}

/// The interfaces, in the object's order: IShellLinkW, IShellLinkA, IPersistFile
const IFACES: usize = 3;

fn base_of(this: u32) -> u32 {
    let ps = mem::ps();
    let found = links(|m| (0..IFACES as u32).map(|i| this.wrapping_sub(ps * i)).find(|b| m.contains_key(b)));
    found.unwrap_or(this)
}
fn with<R>(this: u32, f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let b = base_of(this);
    links(|m| m.get_mut(&b).map(f))
}

fn guid_is(p: u32, d1: u32, rest: [u8; 12]) -> bool {
    mem::r32(p) == d1 && (0..12).all(|i| mem::r8(p + 4 + i) == rest[i as usize])
}
/// {xxxxxxxx-0000-0000-C000-000000000046}
fn ole_guid(p: u32, d1: u32) -> bool {
    guid_is(p, d1, [0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46])
}

/// Which interface an IID asks for: 0 IShellLinkW (or IUnknown), 1 IShellLinkA, 2 IPersistFile
/// (or IPersist)
fn which(iid: u32) -> Option<u32> {
    match () {
        _ if ole_guid(iid, 0) || ole_guid(iid, 0x0002_14f9) => Some(0),
        _ if ole_guid(iid, 0x0002_14ee) => Some(1),
        _ if ole_guid(iid, 0x0000_010b) || ole_guid(iid, 0x0000_010c) => Some(2),
        _ => None,
    }
}

fn qi(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return 0x8000_4003;
    }
    let b = base_of(this);
    match which(iid) {
        Some(i) => {
            with(b, |s| s.refs += 1);
            mem::putp(out, b + mem::ps() * i);
            0
        }
        None => {
            mem::putp(out, 0);
            0x8000_4002
        }
    }
}
fn add_ref(c: &mut Ctx) -> u32 {
    with(c.a(0), |s| {
        s.refs += 1;
        s.refs
    })
    .unwrap_or(1)
}
fn release(c: &mut Ctx) -> u32 {
    let b = base_of(c.a(0));
    links(|m| match m.get_mut(&b) {
        Some(s) if s.refs > 1 => {
            s.refs -= 1;
            s.refs
        }
        Some(_) => {
            m.remove(&b);
            0
        }
        None => 0,
    })
}

/// The string argument i of a W (0) or A (1) method
fn text(c: &mut Ctx, i: u32) -> String {
    let p = c.a(i);
    if p == 0 {
        return String::new();
    }
    arg_str(p, wide(c))
}
fn wide(c: &Ctx) -> bool {
    base_of(c.a(0)) == c.a(0)
}
/// A string into (buffer, characters): truncated as lstrcpyn does
fn put(c: &Ctx, buf: u32, cap: u32, s: &str) {
    if buf == 0 || cap == 0 {
        return;
    }
    if wide(c) {
        mem::put_wstr(buf, cap, &s.encode_utf16().collect::<Vec<_>>());
    } else {
        mem::put_cstr(buf, cap, &crate::text::s2a(s));
    }
}

fn set(c: &mut Ctx, f: impl FnOnce(&mut Link, String)) -> u32 {
    let t = text(c, 1);
    with(c.a(0), |s| {
        f(&mut s.link, t);
        s.dirty = true;
    });
    0
}
fn get(c: &mut Ctx, f: impl FnOnce(&Link) -> String) -> u32 {
    let (buf, cap) = (c.a(1), c.a(2));
    let v = with(c.a(0), |s| f(&s.link)).unwrap_or_default();
    put(c, buf, cap, &v);
    0
}

// IShellLinkW / IShellLinkA
fn GetPath(c: &mut Ctx) -> u32 {
    let (buf, cap, fd) = (c.a(1), c.a(2), c.a(3));
    let p = with(c.a(0), |s| s.link.path.clone()).unwrap_or_default();
    put(c, buf, cap, &p);
    if fd != 0 {
        // WIN32_FIND_DATA: the attributes and the name; times and size unknown
        let size = if wide(c) { 592 } else { 320 };
        mem::fill(fd, size, 0);
        let name = p.rsplit('\\').next().unwrap_or("");
        let attrs = if p.is_empty() { 0 } else { crate::files::resolve(&p).map_or(0x80, |h| if crate::files::is_dir(&h) { 0x10 } else { 0x20 }) };
        mem::w32(fd, attrs);
        put(c, fd + 44, 260, name);
    }
    if p.is_empty() { 1 } else { 0 }
}
fn GetIDList(c: &mut Ctx) -> u32 {
    mem::putp(c.a(1), 0);
    crate::com::E_NOTIMPL
}
fn SetIDList(_: &mut Ctx) -> u32 {
    crate::com::E_NOTIMPL
}
fn GetDescription(c: &mut Ctx) -> u32 {
    get(c, |l| l.desc.clone())
}
fn SetDescription(c: &mut Ctx) -> u32 {
    set(c, |l, t| l.desc = t)
}
fn GetWorkingDirectory(c: &mut Ctx) -> u32 {
    get(c, |l| l.dir.clone())
}
fn SetWorkingDirectory(c: &mut Ctx) -> u32 {
    set(c, |l, t| l.dir = t)
}
fn GetArguments(c: &mut Ctx) -> u32 {
    get(c, |l| l.args.clone())
}
fn SetArguments(c: &mut Ctx) -> u32 {
    set(c, |l, t| l.args = t)
}
fn GetHotkey(c: &mut Ctx) -> u32 {
    let k = with(c.a(0), |s| s.link.hotkey).unwrap_or(0);
    if c.a(1) == 0 {
        return 0x8007_0057;
    }
    mem::w16(c.a(1), k);
    0
}
fn SetHotkey(c: &mut Ctx) -> u32 {
    let k = c.a(1) as u16;
    with(c.a(0), |s| {
        s.link.hotkey = k;
        s.dirty = true;
    });
    0
}
fn GetShowCmd(c: &mut Ctx) -> u32 {
    let v = with(c.a(0), |s| s.link.show).unwrap_or(1);
    if c.a(1) == 0 {
        return 0x8007_0057;
    }
    mem::w32(c.a(1), if v == 0 { 1 } else { v });
    0
}
fn SetShowCmd(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    with(c.a(0), |s| {
        s.link.show = v;
        s.dirty = true;
    });
    0
}
fn GetIconLocation(c: &mut Ctx) -> u32 {
    let (buf, cap, pi) = (c.a(1), c.a(2), c.a(3));
    let (p, i) = with(c.a(0), |s| (s.link.icon.clone(), s.link.icon_index)).unwrap_or_default();
    put(c, buf, cap, &p);
    if pi != 0 {
        mem::w32(pi, i as u32);
    }
    0
}
fn SetIconLocation(c: &mut Ctx) -> u32 {
    let i = c.a(2) as i32;
    set(c, |l, t| {
        l.icon = t;
        l.icon_index = i;
    })
}
fn SetRelativePath(c: &mut Ctx) -> u32 {
    set(c, |l, t| l.rel = t)
}
fn Resolve(_: &mut Ctx) -> u32 {
    0
}
/// SetPath: the full path (a relative one from the current directory)
fn SetPath(c: &mut Ctx) -> u32 {
    let t = text(c, 1);
    if t.is_empty() {
        return 0x8007_0057;
    }
    let full = crate::files::full_path(&t);
    with(c.a(0), |s| {
        s.link.path = full;
        s.dirty = true;
    });
    0
}

// IPersistFile
fn GetClassID(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return 0x8007_0057;
    }
    mem::write(c.a(1), &CLSID);
    0
}
fn IsDirty(c: &mut Ctx) -> u32 {
    if with(c.a(0), |s| s.dirty).unwrap_or(false) { 0 } else { 1 }
}
fn Load(c: &mut Ctx) -> u32 {
    let name = crate::files::full_path(&arg_str(c.a(1), true));
    let Some(bytes) = crate::files::resolve(&name).and_then(|h| kura::fs::read(h).ok()) else {
        return 0x8007_0002;
    };
    let Some(link) = decode(&bytes) else {
        return 0x8000_4005;
    };
    with(c.a(0), |s| {
        s.link = link;
        s.file = Some(name);
        s.dirty = false;
    });
    0
}
fn Save(c: &mut Ctx) -> u32 {
    let (name, remember) = (c.a(1), c.a(2) != 0);
    let (name, remember) = if name == 0 {
        match with(c.a(0), |s| s.file.clone()).flatten() {
            Some(f) => (f, false),
            None => return 0,
        }
    } else {
        (crate::files::full_path(&arg_str(name, true)), remember)
    };
    let link = with(c.a(0), |s| s.link.clone()).unwrap_or_default();
    let host = crate::files::resolve_new(&name);
    if kura::fs::write(&host, encode(&link)).is_err() {
        return 0x8003_0003; // STG_E_PATHNOTFOUND
    }
    with(c.a(0), |s| {
        if remember {
            s.file = Some(name.clone());
        }
        s.dirty = false;
    });
    0
}
fn SaveCompleted(_: &mut Ctx) -> u32 {
    0
}
fn GetCurFile(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    match with(c.a(0), |s| s.file.clone()).flatten() {
        Some(f) => {
            let w: Vec<u16> = f.encode_utf16().chain([0]).collect();
            // (CoTaskMemAlloc's heap)
            let p = mem::heap_alloc(2 * w.len() as u32, false);
            for (i, &u) in w.iter().enumerate() {
                mem::w16(p + 2 * i as u32, u);
            }
            mem::putp(out, p);
            0
        }
        None => {
            mem::putp(out, 0);
            1
        }
    }
}

fn vtables() -> [u32; IFACES] {
    static V: std::sync::OnceLock<[u32; IFACES]> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        let link = |name: &str| {
            crate::api::vtable(
                name,
                &[
                    (qi, 12),
                    (add_ref, 4),
                    (release, 4),
                    (GetPath, 20),
                    (GetIDList, 8),
                    (SetIDList, 8),
                    (GetDescription, 12),
                    (SetDescription, 8),
                    (GetWorkingDirectory, 12),
                    (SetWorkingDirectory, 8),
                    (GetArguments, 12),
                    (SetArguments, 8),
                    (GetHotkey, 8),
                    (SetHotkey, 8),
                    (GetShowCmd, 8),
                    (SetShowCmd, 8),
                    (GetIconLocation, 16),
                    (SetIconLocation, 12),
                    (SetRelativePath, 12),
                    (Resolve, 12),
                    (SetPath, 8),
                ],
            )
        };
        let pf = crate::api::vtable("IPersistFile", &[(qi, 12), (add_ref, 4), (release, 4), (GetClassID, 8), (IsDirty, 4), (Load, 12), (Save, 12), (SaveCompleted, 8), (GetCurFile, 8)]);
        [link("IShellLinkW"), link("IShellLinkA"), pf]
    })
}

/// CoCreateInstance(CLSID_ShellLink): None for another class, else the HRESULT
pub fn create_instance(clsid: u32, iid: u32, out: u32) -> Option<u32> {
    if !ole_guid(clsid, 0x0002_1401) {
        return None;
    }
    let Some(i) = which(iid) else {
        mem::putp(out, 0);
        return Some(0x8000_4002);
    };
    let ps = mem::ps();
    let o = mem::heap_alloc(ps * IFACES as u32, true);
    for (k, v) in vtables().iter().enumerate() {
        mem::wp(o + ps * k as u32, *v);
    }
    links(|m| m.insert(o, State { link: Link::default(), file: None, dirty: false, refs: 1 }));
    mem::putp(out, o + ps * i);
    Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sample() -> Link {
        Link {
            path: "C:\\MyProg\\MyProg.exe".into(),
            args: "/x \"y z\"".into(),
            dir: "C:\\MyProg".into(),
            desc: "マイプログラム".into(),
            rel: String::new(),
            icon: "C:\\MyProg\\MyProg.exe".into(),
            icon_index: -3,
            show: 7,
            hotkey: 0x0641,
        }
    }

    #[test]
    fn header_is_a_shell_link() {
        let b = encode(&sample());
        assert_eq!(&b[0..4], &[0x4c, 0, 0, 0]);
        assert_eq!(&b[4..20], &CLSID);
        let flags = u32::from_le_bytes(b[20..24].try_into().unwrap());
        assert_eq!(flags, HAS_LINK_INFO | HAS_NAME | HAS_WORKING_DIR | HAS_ARGUMENTS | HAS_ICON_LOCATION | IS_UNICODE);
        assert_eq!(&b[b.len() - 4..], &[0; 4]);
    }

    #[test]
    fn round_trip() {
        assert_eq!(decode(&encode(&sample())).unwrap(), sample());
        // a new link shows normally
        assert_eq!(decode(&encode(&Link::default())).unwrap(), Link { show: 1, ..Link::default() });
    }

    /// A link with the Unicode LinkInfo paths (a 0x24-byte header) and an ID list before it, as
    /// Windows writes for paths outside the code page
    #[test]
    fn unicode_link_info_and_id_list() {
        let mut b = encode(&Link { path: "X".into(), ..sample() });
        b[20] |= HAS_ID_LIST as u8;
        b.splice(0x4c..0x4c, [4, 0, 0xaa, 0xbb, 0, 0]);
        let info = 0x4c + 6;
        let old = u32::from_le_bytes(b[info..info + 4].try_into().unwrap()) as usize;
        let path: Vec<u8> = "C:\\ゲーム\\a.exe".encode_utf16().chain([0, 0]).flat_map(|u| u.to_le_bytes()).collect();
        let mut li = Vec::new();
        let (base_w, suffix_w) = (0x24 + 17 + 2 + 1, 0x24 + 17 + 2 + 1 + path.len() as u32);
        for v in [suffix_w + 2, 0x24, 1, 0x24, 0x24 + 17, 0, 0x24 + 17 + 1, base_w, suffix_w] {
            li.extend_from_slice(&v.to_le_bytes());
        }
        li.extend([0x11, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0x10, 0, 0, 0, 0]);
        li.extend([b'?', 0, 0]);
        li.extend(&path);
        li.extend([0, 0]);
        b.splice(info..info + old, li);
        let l = decode(&b).unwrap();
        assert_eq!(l.path, "C:\\ゲーム\\a.exe");
        assert_eq!(l.args, sample().args);
    }

    #[test]
    fn not_links() {
        assert!(decode(b"").is_none());
        assert!(decode(&[0x4c, 0, 0, 0]).is_none());
        let mut b = encode(&sample());
        b.truncate(b.len() - 10);
        assert!(decode(&b).is_none());
    }

    proptest! {
        #[test]
        fn any_link_round_trips(path in "[A-Za-z]:\\\\[a-z ]{0,12}", args in "\\PC{0,20}", dir in "\\PC{0,20}", desc in "\\PC{0,20}", rel in "\\PC{0,8}", icon in "\\PC{0,20}", icon_index: i32, show in 1u32..12, hotkey: u16) {
            let l = Link { path, args, dir, desc, rel, icon, icon_index, show, hotkey };
            prop_assert_eq!(decode(&encode(&l)).unwrap(), l);
        }

        #[test]
        fn damaged_links_do_not_panic(at in any::<prop::sample::Index>(), v: u8, cut in any::<prop::sample::Index>()) {
            let mut b = encode(&sample());
            let i = at.index(b.len());
            b[i] = v;
            b.truncate(cut.index(b.len() + 1).max(1));
            let _ = decode(&b);
        }
    }
}
