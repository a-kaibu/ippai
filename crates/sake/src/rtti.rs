//! MSVC RTTI: __RTDynamicCast, __RTCastToVoid and __RTtypeid of msvcrt / vcruntime140 (x86
//! and x64). The data structures are MSVC's (rttidata.h): an object's vtable has its complete
//! object locator just before it, which names the class's type descriptor and its class
//! hierarchy (its base classes, each with the displacement from the complete object). On x64
//! the locator's pointers are RVAs from the image base, which the locator's own RVA gives.
//! The algorithm follows Wine's msvcrt (cpp.c): single inheritance takes the first base class
//! of the target type; multiple inheritance looks for the source subobject at the given offset
//! first and needs the target to be a public base reachable from it or the complete object.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::mem::{self, r32};

/// A class hierarchy as the locator gives it.
#[derive(Debug, Clone, PartialEq)]
pub struct Base {
    /// the type descriptor's address (its decorated name is compared in place: `same_type`)
    pub td: u32,
    /// the bases this one contains (itself included)
    pub contained: u32,
    /// PMD: member displacement, vbtable displacement (-1: not virtual), offset in the vbtable
    pub mdisp: i32,
    pub pdisp: i32,
    pub vdisp: i32,
    /// BCD_* attributes (1 not visible, 2 ambiguous, 4 private or protected...)
    pub attrs: u32,
}

/// (complete object, the type descriptor of the complete class, its attributes, its bases):
/// what `obj`'s vtable leads to (None: no locator).
fn locate(obj: u32) -> Option<(u32, u32, u32, Vec<Base>)> {
    let ps = mem::ps();
    let vt = mem::rp(obj);
    if vt < ps {
        return None;
    }
    let col = mem::rp(vt - ps);
    if col == 0 {
        return None;
    }
    let x64 = mem::x64();
    // (x64: signature 1 and pSelf, the locator's RVA)
    let base = if x64 { col.wrapping_sub(r32(col + 20)) } else { 0 };
    let at = |v: u32| if x64 { base.wrapping_add(v) } else { v };
    let (offset, cd_offset) = (r32(col + 4), r32(col + 8));
    let td = at(r32(col + 12));
    let chd = at(r32(col + 16));
    let mut complete = obj.wrapping_sub(offset);
    if cd_offset != 0 {
        complete = complete.wrapping_add(r32(obj.wrapping_sub(cd_offset)));
    }
    let (attrs, n, arr) = (r32(chd + 4), r32(chd + 8), at(r32(chd + 12)));
    let mut bases = Vec::with_capacity(n.min(256) as usize);
    for i in 0..n.min(256) {
        let bcd = at(r32(arr + 4 * i));
        let td = at(r32(bcd));
        bases.push(Base {
            td,
            contained: r32(bcd + 4),
            mdisp: r32(bcd + 8) as i32,
            pdisp: r32(bcd + 12) as i32,
            vdisp: r32(bcd + 16) as i32,
            attrs: r32(bcd + 20),
        });
    }
    Some((complete, td, attrs, bases))
}

/// Whether two type descriptors name the same type: the same descriptor, or the same decorated
/// name (after each one's vtable pointer and spare pointer; one class's descriptor is in every
/// module that uses it). MSVC's order: the pointers first, the names only when they differ, and
/// those in place, byte by byte up to 1024, with no copy.
pub fn same_type(a: u32, b: u32) -> bool {
    if a == b {
        return true;
    }
    let (p, q) = (a.wrapping_add(2 * mem::ps()), b.wrapping_add(2 * mem::ps()));
    for i in 0..1024 {
        let (x, y) = (mem::r8(p.wrapping_add(i)), mem::r8(q.wrapping_add(i)));
        if x != y {
            return false;
        }
        if x == 0 {
            return true;
        }
    }
    true
}

/// The address of base `b` in the complete object `obj` (PMD adjustment; a virtual base
/// through the object's vbtable).
fn adjust(obj: u32, b: &Base) -> u32 {
    let mut p = obj.wrapping_add(b.mdisp as u32);
    if b.pdisp >= 0 {
        let vbt = mem::rp(obj.wrapping_add(b.pdisp as u32));
        p = p.wrapping_add(r32(vbt.wrapping_add(b.vdisp as u32))).wrapping_add(b.pdisp as u32);
    }
    p
}

const CHD_MULTINH: u32 = 1;
/// BCD_NOTVISIBLE | BCD_AMBIGUOUS | BCD_PRIVORPROTBASE: a base a cast may not go to
const BCD_HIDDEN: u32 = 1 | 2 | 4;

/// dynamic_cast's choice of the target subobject: `bases` is the hierarchy (the complete
/// class first), `same` tells a base of the target type and `is_src` the source subobject (its
/// type at the source pointer's offset). The result is the index of the target base.
pub fn pick(bases: &[Base], attrs: u32, same: &dyn Fn(&Base) -> bool, is_src: &dyn Fn(&Base) -> bool) -> Option<usize> {
    if attrs & CHD_MULTINH == 0 {
        // single inheritance: the first base of the target type
        return bases.iter().position(same);
    }
    // multiple inheritance: from a public source subobject, a downcast or cross cast to a
    // visible, unambiguous target in the complete object; else an upcast within the source
    // subobject's own bases
    let src = bases.iter().position(is_src);
    if src.is_none_or(|s| bases[s].attrs & (1 | 4) == 0) {
        if let Some(i) = bases.iter().position(|b| same(b) && b.attrs & BCD_HIDDEN == 0) {
            return Some(i);
        }
    }
    let src = src?;
    let end = (src + bases[src].contained as usize).min(bases.len());
    (src + 1..end).find(|&i| same(&bases[i]) && bases[i].attrs & 2 == 0)
}

fn ret_ptr(c: &mut Ctx, p: u32) -> u32 {
    if mem::x64() {
        return c.ret64(mem::widen(p));
    }
    p
}

/// __RTDynamicCast(object, vfptr offset, source type, target type, is a reference)
fn __RTDynamicCast(c: &mut Ctx) -> u32 {
    let (obj, vf_delta, source, target, is_ref) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if obj == 0 {
        return ret_ptr(c, 0);
    }
    let r = locate(obj).and_then(|(complete, _, attrs, bases)| {
        let same = |b: &Base| same_type(b.td, target);
        let src = obj.wrapping_sub(vf_delta);
        let is_src = |b: &Base| adjust(complete, b) == src && same_type(b.td, source);
        pick(&bases, attrs, &same, &is_src).map(|i| adjust(complete, &bases[i]))
    });
    if r.is_none() && is_ref != 0 {
        // dynamic_cast<T&> that fails throws std::bad_cast (a reference cannot be null)
        return crate::msvcp::throw_bad_cast(c);
    }
    ret_ptr(c, r.unwrap_or(0))
}

/// __RTCastToVoid(object): its complete object
fn __RTCastToVoid(c: &mut Ctx) -> u32 {
    let obj = c.a(0);
    let r = if obj == 0 { 0 } else { locate(obj).map_or(0, |l| l.0) };
    ret_ptr(c, r)
}

/// __RTtypeid(object): the type descriptor of its complete class
fn __RTtypeid(c: &mut Ctx) -> u32 {
    let obj = c.a(0);
    let r = if obj == 0 { 0 } else { locate(obj).map_or(0, |l| l.1) };
    ret_ptr(c, r)
}

pub fn init() {
    for d in ["msvcrt", "ucrtbase", "vcruntime140"] {
        implement(d, "__RTDynamicCast", __RTDynamicCast);
        implement(d, "__RTCastToVoid", __RTCastToVoid);
        implement(d, "__RTtypeid", __RTtypeid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (a type stands for its descriptor by a hash of its name here)
    fn id(name: &str) -> u32 {
        name.bytes().fold(2166136261u32, |h, c| (h ^ c as u32).wrapping_mul(16777619))
    }
    fn b(name: &str, contained: u32, mdisp: i32, attrs: u32) -> Base {
        Base { td: id(name), contained, mdisp, pdisp: -1, vdisp: 0, attrs }
    }
    /// a cast from the subobject of type `src` at offset `off`
    fn run(bases: &[Base], attrs: u32, src: (&str, i32), target: &str) -> Option<usize> {
        let (t, s) = (id(target), id(src.0));
        pick(bases, attrs, &|x: &Base| x.td == t, &|x: &Base| x.td == s && x.mdisp == src.1)
    }

    /// review b09: descriptors compare by pointer, then by name in place (two modules' copies
    /// of one class), and never past 1024 bytes
    #[test]
    fn same_type_by_pointer_then_name() {
        let _g = crate::test_guest::enter();
        let td = |name: &[u8]| {
            let p = mem::heap_alloc(2 * mem::ps() + name.len() as u32 + 1, true);
            mem::write(p + 2 * mem::ps(), name);
            p
        };
        let (a, a2, bb) = (td(b".?AVDog@@"), td(b".?AVDog@@"), td(b".?AVDogs@@"));
        assert!(same_type(a, a) && same_type(a, a2) && same_type(a2, a));
        assert!(!same_type(a, bb) && !same_type(bb, a));
        let (l1, l2) = (td(&[b'x'; 3000]), td(&[b'x'; 3000]));
        assert!(same_type(l1, l2));
        for p in [a, a2, bb, l1, l2] {
            mem::heap_free(p);
        }
    }

    use proptest::prelude::*;
    proptest! {
        /// same_type agrees with comparing the two names (cut at 1024 bytes) as copies
        #[test]
        fn same_type_is_name_equality(x in proptest::collection::vec(1u8..=255, 0..40), y in proptest::collection::vec(1u8..=255, 0..40), share in any::<bool>()) {
            let _g = crate::test_guest::enter();
            let td = |name: &[u8]| {
                let p = mem::heap_alloc(2 * mem::ps() + name.len() as u32 + 1, true);
                mem::write(p + 2 * mem::ps(), name);
                p
            };
            let y = if share { x.clone() } else { y };
            let (a, b) = (td(&x), td(&y));
            prop_assert_eq!(same_type(a, b), x == y);
            mem::heap_free(a);
            mem::heap_free(b);
        }
    }

    #[test]
    fn single_inheritance_takes_the_first_base_of_the_target() {
        // class C : B : A
        let h = [b(".?AVC@@", 3, 0, 0), b(".?AVB@@", 2, 0, 0), b(".?AVA@@", 1, 0, 0)];
        assert_eq!(run(&h, 0, (".?AVA@@", 0), ".?AVB@@"), Some(1));
        assert_eq!(run(&h, 0, (".?AVA@@", 0), ".?AVC@@"), Some(0));
        assert_eq!(run(&h, 0, (".?AVA@@", 0), ".?AVX@@"), None);
    }

    #[test]
    fn multiple_inheritance_cross_casts_and_refuses_hidden_bases() {
        // class C : A, B (B at 8); a private base P at 16
        let h = [b(".?AVC@@", 4, 0, 0), b(".?AVA@@", 1, 0, 0), b(".?AVB@@", 1, 8, 0), b(".?AVP@@", 1, 16, 4)];
        // from A (offset 0) to B: the complete object's B
        assert_eq!(run(&h, CHD_MULTINH, (".?AVA@@", 0), ".?AVB@@"), Some(2));
        // a private base is not a target from outside
        assert_eq!(run(&h, CHD_MULTINH, (".?AVA@@", 0), ".?AVP@@"), None);
        // from a private base no downcast either
        assert_eq!(run(&h, CHD_MULTINH, (".?AVP@@", 16), ".?AVC@@"), None);
        // an ambiguous base neither
        let amb = [b(".?AVC@@", 3, 0, 0), b(".?AVA@@", 1, 0, 2), b(".?AVA@@", 1, 8, 2)];
        assert_eq!(run(&amb, CHD_MULTINH, (".?AVA@@", 0), ".?AVA@@"), None);
        // an upcast inside the source: from B (at 8) to its own base D, hidden from C
        let up = [b(".?AVC@@", 4, 0, 0), b(".?AVA@@", 1, 0, 0), b(".?AVB@@", 2, 8, 0), b(".?AVD@@", 1, 8, 1)];
        assert_eq!(run(&up, CHD_MULTINH, (".?AVB@@", 8), ".?AVD@@"), Some(3));
    }
}
