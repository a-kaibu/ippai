//! GDI+ font families and collections (gdiplus.dll): the installed collection (gdi.rs's faces
//! and the fonts the game added), private collections of font files and memory fonts,
//! families by name with their design metrics (em height, cell ascent and descent, line
//! spacing), the generic families. gdiplus_text's fonts are made of these families.
//!
//! A family is one object for good: asking for it again, cloning it or getting a font's gives
//! the same pointer (Wine's tests check it), and deleting it does nothing. The families every
//! Windows has (Tahoma, Arial, Times New Roman, ...) are there by their names even without
//! their fonts, drawn and measured with a built-in face, so the games that ask for them by
//! name get text.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx, Handler};
use crate::mem::{self, r16, r32, w16, w32};
use crate::HashMap;
use std::sync::Mutex;

const OK: u32 = 0;
const INVALID_PARAMETER: u32 = 2;
const FILE_NOT_FOUND: u32 = 10;
const FONT_FAMILY_NOT_FOUND: u32 = 14;

/// LF_FACESIZE
const FACESIZE: usize = 32;

#[derive(Clone)]
pub(crate) struct Family {
    /// what GdipGetFamilyName says
    pub name: String,
    /// gdi.rs's face that has its glyphs and metrics
    pub face: usize,
}

#[derive(Default)]
struct State {
    families: HashMap<u32, Family>,
    /// the installed collection's handle and its families by face (or substitute name)
    installed: u32,
    by_face: HashMap<usize, u32>,
    by_name: HashMap<String, u32>,
    /// private collections: their families
    private: HashMap<u32, Vec<u32>>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(STATE.lock().unwrap().get_or_insert_with(State::default))
}

/// The families Windows always has, absent from sake's fonts: (name, built-in face drawn with:
/// 0 MS Gothic, 1 MS PGothic, 3 MS PMincho, 4 MS UI Gothic)
const ALWAYS: &[(&str, usize)] = &[
    ("Arial", 1),
    ("Tahoma", 1),
    ("Microsoft Sans Serif", 1),
    ("Segoe UI", 1),
    ("Verdana", 1),
    ("Meiryo", 1),
    ("Meiryo UI", 4),
    ("Yu Gothic", 1),
    ("Times New Roman", 3),
    ("Courier New", 0),
];
/// names that are another family's (GDI's substitutes): MS Shell Dlg is MS UI Gothic on a
/// Japanese Windows
const ALIASES: &[(&str, &str)] = &[("MS Shell Dlg", "MS UI Gothic"), ("MS Shell Dlg 2", "MS UI Gothic")];

fn new_family(s: &mut State, f: Family) -> u32 {
    let h = mem::heap_alloc(16, true);
    s.families.insert(h, f);
    h
}
/// The installed family of built-in or added face `face`
fn face_family(s: &mut State, face: usize) -> u32 {
    if let Some(&h) = s.by_face.get(&face) {
        return h;
    }
    let name = crate::gdi::face_names(face).into_iter().next().unwrap_or_default();
    let h = new_family(s, Family { name, face });
    s.by_face.insert(face, h);
    h
}
/// The installed family named `name` (any of its names, any case), None: not there
fn installed_family(name: &str) -> Option<u32> {
    let name = ALIASES.iter().find(|a| a.0.eq_ignore_ascii_case(name)).map_or(name, |a| a.1);
    if let Some(face) = crate::gdi::find_face(name) {
        return Some(with(|s| face_family(s, face)));
    }
    let &(n, face) = ALWAYS.iter().find(|a| a.0.eq_ignore_ascii_case(name))?;
    Some(with(|s| {
        if let Some(&h) = s.by_name.get(n) {
            return h;
        }
        let h = new_family(s, Family { name: n.to_string(), face });
        s.by_name.insert(n.to_string(), h);
        h
    }))
}
/// The installed families: every face's, then the ones Windows always has
fn installed_list() -> Vec<u32> {
    let mut v: Vec<u32> = (0..crate::gdi::face_count()).map(|i| with(|s| face_family(s, i))).collect();
    for (n, _) in ALWAYS {
        if let Some(h) = installed_family(n) {
            if !v.contains(&h) {
                v.push(h);
            }
        }
    }
    v
}

/// Whether GDI+ has a family of this name (built-in, added or always there)
pub(crate) fn is_family_name(name: &str) -> bool {
    ALIASES.iter().any(|a| a.0.eq_ignore_ascii_case(name)) || crate::gdi::find_face(name).is_some() || stand_in_face(name).is_some()
}

/// The built-in face that stands in for a family every Windows has (None: not one of them)
pub(crate) fn stand_in_face(name: &str) -> Option<usize> {
    ALWAYS.iter().find(|a| a.0.eq_ignore_ascii_case(name)).map(|a| a.1)
}

pub(crate) fn family(h: u32) -> Option<Family> {
    with(|s| s.families.get(&h).cloned())
}
/// The family a LOGFONT's face name stands for: the installed one of that name, or Arial
/// (GDI+'s fallback for a face it does not have)
pub(crate) fn family_for(face: &str) -> u32 {
    installed_family(face).or_else(|| installed_family("Arial")).unwrap_or(0)
}

/// A family's design metrics: em height, cell ascent, cell descent, line spacing (the
/// larger of the Windows ascent + descent and hhea's ascent + descent + line gap)
pub(crate) fn design_metrics(face: usize) -> [u16; 4] {
    match crate::gdi::sys_face_design_metrics(face) {
        Some(m) => [m[0], m[1], m[2], m[1].saturating_add(m[2]).saturating_add(m[3])],
        None => [2048, 1802, 246, 2048],
    }
}

fn wstr(p: u32) -> String {
    let mut v = Vec::new();
    let mut a = p;
    loop {
        let c = r16(a);
        if c == 0 || v.len() > 4096 {
            break;
        }
        v.push(c);
        a += 2;
    }
    String::from_utf16_lossy(&v)
}

fn is_collection(h: u32) -> bool {
    with(|s| h != 0 && (h == s.installed || s.private.contains_key(&h)))
}
/// a collection's families
fn members(h: u32) -> Option<Vec<u32>> {
    if with(|s| h != 0 && h == s.installed) {
        return Some(installed_list());
    }
    with(|s| s.private.get(&h).cloned())
}

/// GdipCreateFontFamilyFromName(name, collection, &family): in the collection given, else
/// the installed one
fn GdipCreateFontFamilyFromName(c: &mut Ctx) -> u32 {
    let (np, coll, out) = (c.a(0), c.a(1), c.a(2));
    if np == 0 || out == 0 {
        return INVALID_PARAMETER;
    }
    let name = wstr(np);
    let private = with(|s| s.private.get(&coll).cloned());
    let found = match private {
        Some(list) => list.into_iter().find(|&h| family(h).is_some_and(|f| crate::gdi::face_names(f.face).iter().chain([&f.name]).any(|n| n.eq_ignore_ascii_case(&name)))),
        None => installed_family(&name),
    };
    let Some(h) = found else { return FONT_FAMILY_NOT_FOUND };
    mem::putp(out, h);
    OK
}
fn GdipCloneFontFamily(c: &mut Ctx) -> u32 {
    let (h, out) = (c.a(0), c.a(1));
    if family(h).is_none() || out == 0 {
        return INVALID_PARAMETER;
    }
    mem::putp(out, h);
    OK
}
fn GdipDeleteFontFamily(c: &mut Ctx) -> u32 {
    if family(c.a(0)).is_none() {
        return INVALID_PARAMETER;
    }
    OK
}
/// GdipGetFamilyName(family, name[LF_FACESIZE], language): a NULL buffer is fine
fn GdipGetFamilyName(c: &mut Ctx) -> u32 {
    let (h, out) = (c.a(0), c.a(1));
    let Some(f) = family(h) else { return INVALID_PARAMETER };
    if out != 0 {
        let u: Vec<u16> = f.name.encode_utf16().take(FACESIZE - 1).collect();
        for (i, ch) in u.iter().enumerate() {
            w16(out + 2 * i as u32, *ch);
        }
        w16(out + 2 * u.len() as u32, 0);
    }
    OK
}
/// Get{EmHeight,CellAscent,CellDescent,LineSpacing}(family, style, &UINT16)
fn metric(c: &mut Ctx, k: usize) -> u32 {
    let (h, out) = (c.a(0), c.a(2));
    let Some(f) = family(h) else { return INVALID_PARAMETER };
    if out == 0 {
        return INVALID_PARAMETER;
    }
    w16(out, design_metrics(f.face)[k]);
    OK
}
fn GdipGetEmHeight(c: &mut Ctx) -> u32 {
    metric(c, 0)
}
fn GdipGetCellAscent(c: &mut Ctx) -> u32 {
    metric(c, 1)
}
fn GdipGetCellDescent(c: &mut Ctx) -> u32 {
    metric(c, 2)
}
fn GdipGetLineSpacing(c: &mut Ctx) -> u32 {
    metric(c, 3)
}
fn GdipIsStyleAvailable(c: &mut Ctx) -> u32 {
    let (h, out) = (c.a(0), c.a(2));
    if family(h).is_none() || out == 0 {
        return INVALID_PARAMETER;
    }
    w32(out, 1);
    OK
}
fn generic(c: &mut Ctx, name: &str) -> u32 {
    let out = c.a(0);
    if out == 0 {
        return INVALID_PARAMETER;
    }
    let Some(h) = installed_family(name) else { return FONT_FAMILY_NOT_FOUND };
    mem::putp(out, h);
    OK
}
fn GdipGetGenericFontFamilySansSerif(c: &mut Ctx) -> u32 {
    generic(c, "Microsoft Sans Serif")
}
fn GdipGetGenericFontFamilySerif(c: &mut Ctx) -> u32 {
    generic(c, "Times New Roman")
}
fn GdipGetGenericFontFamilyMonospace(c: &mut Ctx) -> u32 {
    generic(c, "Courier New")
}

/// GdipNewInstalledFontCollection(&collection): always the same one
fn GdipNewInstalledFontCollection(c: &mut Ctx) -> u32 {
    let out = c.a(0);
    if out == 0 {
        return INVALID_PARAMETER;
    }
    let h = with(|s| {
        if s.installed == 0 {
            s.installed = mem::heap_alloc(16, true);
        }
        s.installed
    });
    mem::putp(out, h);
    OK
}
fn GdipNewPrivateFontCollection(c: &mut Ctx) -> u32 {
    let out = c.a(0);
    if out == 0 {
        return INVALID_PARAMETER;
    }
    let h = mem::heap_alloc(16, true);
    with(|s| s.private.insert(h, Vec::new()));
    mem::putp(out, h);
    OK
}
/// GdipDeletePrivateFontCollection(&collection): NULLs the pointer; its families stay
fn GdipDeletePrivateFontCollection(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if p == 0 {
        return INVALID_PARAMETER;
    }
    let h = mem::rp(p);
    if with(|s| s.private.remove(&h)).is_none() {
        return INVALID_PARAMETER;
    }
    mem::heap_free(h);
    mem::wp(p, 0);
    OK
}
/// a font's data into a private collection: a family for it (one per family name)
fn add_private(coll: u32, data: Vec<u8>) -> u32 {
    let Some(face) = crate::gdi::add_font_data(data) else { return FILE_NOT_FOUND };
    with(|s| {
        let fam = match s.by_face.get(&face) {
            Some(&h) => h,
            None => face_family(s, face),
        };
        // (the collection deleted by another thread meanwhile: InvalidParameter, not an abort)
        let Some(list) = s.private.get_mut(&coll) else { return INVALID_PARAMETER };
        if !list.contains(&fam) {
            list.push(fam);
        }
        OK
    })
}
fn GdipPrivateAddFontFile(c: &mut Ctx) -> u32 {
    let (coll, np) = (c.a(0), c.a(1));
    if np == 0 || !with(|s| s.private.contains_key(&coll)) {
        return INVALID_PARAMETER;
    }
    let path = wstr(np);
    let Some(data) = crate::files::resolve(&path).and_then(|h| kura::fs::read(h).ok()) else { return FILE_NOT_FOUND };
    add_private(coll, data)
}
fn GdipPrivateAddMemoryFont(c: &mut Ctx) -> u32 {
    let (coll, p, n) = (c.a(0), c.a(1), c.i(2));
    if p == 0 || n <= 0 || !with(|s| s.private.contains_key(&coll)) {
        return INVALID_PARAMETER;
    }
    add_private(coll, mem::slice(p, n as u32).to_vec())
}
fn GdipGetFontCollectionFamilyCount(c: &mut Ctx) -> u32 {
    let (h, out) = (c.a(0), c.a(1));
    let Some(m) = members(h).filter(|_| out != 0) else { return INVALID_PARAMETER };
    w32(out, m.len() as u32);
    OK
}
/// GdipGetFontCollectionFamilyList(collection, sought, families[], &found)
fn GdipGetFontCollectionFamilyList(c: &mut Ctx) -> u32 {
    let (h, n, out, found) = (c.a(0), c.i(1), c.a(2), c.a(3));
    if out == 0 || found == 0 || n < 0 || !is_collection(h) {
        return INVALID_PARAMETER;
    }
    let m = members(h).unwrap_or_default();
    let k = m.len().min(n as usize);
    for (i, f) in m.iter().take(k).enumerate() {
        let step = if mem::x64() { 8 } else { 4 };
        mem::wp(out + step * i as u32, *f);
    }
    w32(found, k as u32);
    OK
}

pub fn init() {
    let fns: &[(&str, Handler)] = &[
        ("GdipCreateFontFamilyFromName", GdipCreateFontFamilyFromName),
        ("GdipCloneFontFamily", GdipCloneFontFamily),
        ("GdipDeleteFontFamily", GdipDeleteFontFamily),
        ("GdipGetFamilyName", GdipGetFamilyName),
        ("GdipGetEmHeight", GdipGetEmHeight),
        ("GdipGetCellAscent", GdipGetCellAscent),
        ("GdipGetCellDescent", GdipGetCellDescent),
        ("GdipGetLineSpacing", GdipGetLineSpacing),
        ("GdipIsStyleAvailable", GdipIsStyleAvailable),
        ("GdipGetGenericFontFamilySansSerif", GdipGetGenericFontFamilySansSerif),
        ("GdipGetGenericFontFamilySerif", GdipGetGenericFontFamilySerif),
        ("GdipGetGenericFontFamilyMonospace", GdipGetGenericFontFamilyMonospace),
        ("GdipNewInstalledFontCollection", GdipNewInstalledFontCollection),
        ("GdipNewPrivateFontCollection", GdipNewPrivateFontCollection),
        ("GdipDeletePrivateFontCollection", GdipDeletePrivateFontCollection),
        ("GdipPrivateAddFontFile", GdipPrivateAddFontFile),
        ("GdipPrivateAddMemoryFont", GdipPrivateAddMemoryFont),
        ("GdipGetFontCollectionFamilyCount", GdipGetFontCollectionFamilyCount),
        ("GdipGetFontCollectionFamilyList", GdipGetFontCollectionFamilyList),
    ];
    for (n, f) in fns {
        implement("gdiplus", n, *f);
    }
    let _ = r32;
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod review_b06_tests {
    use super::*;

    /// A font added to a collection another thread deleted meanwhile (after the caller's
    /// check): InvalidParameter, not the abort of an unwrap
    #[test]
    fn a_font_into_a_deleted_collection_is_refused() {
        let Some(font) = std::fs::read_dir("/usr/share/fonts/truetype").ok().into_iter().flatten().flatten().flat_map(|d| std::fs::read_dir(d.path()).ok().into_iter().flatten().flatten()).map(|e| e.path()).find(|p| p.extension().is_some_and(|x| x == "ttf")) else {
            eprintln!("no TrueType font on this machine: skipped");
            return;
        };
        let _g = crate::test_guest::enter();
        let data = std::fs::read(font).unwrap();
        assert_eq!(add_private(0x7fff_0010, data), INVALID_PARAMETER);
    }
}
