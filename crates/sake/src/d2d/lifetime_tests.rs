//! Object lifetimes (review B-2): every D2D object's state goes with its last Release, and what
//! an object holds (a context its device and target, a device its factory, a geometry its
//! factory and source, a sink its geometry) is released with it, as on Windows.
use super::*;
use crate::test_guest::{call, enter};
use proptest::prelude::*;

fn out() -> u32 {
    mem::heap_alloc(8, true)
}
fn made(f: Handler, args: &[u32]) -> u32 {
    let o = out();
    let mut a = args.to_vec();
    a.push(o);
    assert_eq!(call(f, &a), 0);
    let p = r32(o);
    mem::heap_free(o);
    assert!(com::refs(p).is_some());
    p
}
fn floats(v: &[f32]) -> u32 {
    let p = mem::heap_alloc(4 * v.len() as u32, true);
    for (i, x) in v.iter().enumerate() {
        w32(p + 4 * i as u32, x.to_bits());
    }
    p
}

#[test]
fn brushes_and_stops_go_with_their_last_release() {
    let _g = enter();
    let f = factory();
    let d = made(CreateDevice, &[f, 0]);
    let dc = made(CreateDeviceContext, &[d, 0]);
    let color = floats(&[1.0, 0.5, 0.0, 1.0]);
    let b = made(CreateSolidColorBrush, &[dc, color, 0]);
    assert!(st(|s| s.brushes.contains_key(&b)));
    com::add_ref_obj(b);
    assert_eq!(com::release_obj(b), 1);
    assert!(st(|s| s.brushes.contains_key(&b)));
    assert_eq!(com::release_obj(b), 0);
    assert!(!st(|s| s.brushes.contains_key(&b)));
    assert_eq!(com::refs(b), None);
    // (a gradient brush copies its stops: either goes on its own)
    let stops = floats(&[0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0]);
    let sc = made(CreateGradientStopCollection, &[dc, stops, 2, 0, 0]);
    let lp = floats(&[0.0, 0.0, 10.0, 0.0]);
    let lb = made(CreateLinearGradientBrush, &[dc, lp, 0, sc]);
    assert_eq!(com::release_obj(sc), 0);
    assert!(!st(|s| s.stops.contains_key(&sc)));
    assert!(st(|s| s.brushes.get(&lb).is_some_and(|b| b.linear.as_ref().is_some_and(|l| l.2.len() == 2))));
    assert_eq!(com::release_obj(lb), 0);
    assert!(!st(|s| s.brushes.contains_key(&lb)));
    for p in [color, stops, lp] {
        mem::heap_free(p);
    }
    assert_eq!(call(DcRelease, &[dc]), 0);
    assert_eq!(com::release_obj(d), 0);
    assert_eq!(com::release_obj(f), 0);
}

/// A context keeps its device and its target (SetTarget AddRefs, a new target or the context's
/// last Release lets the old one go); a device keeps its factory.
#[test]
fn contexts_hold_their_device_and_target() {
    let _g = enter();
    let f = factory();
    let d = made(CreateDevice, &[f, 0]);
    assert_eq!(com::refs(f), Some(2));
    let dc = made(CreateDeviceContext, &[d, 0]);
    assert_eq!(com::refs(d), Some(2));
    let (b1, b2) = (new_bitmap(0x5000), new_bitmap(0x6000));
    assert_eq!(call(SetTarget, &[dc, b1]), 0);
    assert_eq!(com::refs(b1), Some(2));
    // (the guest lets its reference go: the context's keeps it)
    assert_eq!(call(BitmapRelease, &[b1]), 1);
    assert!(st(|s| s.bitmaps.contains_key(&b1)));
    assert_eq!(call(SetTarget, &[dc, b2]), 0);
    assert_eq!(com::refs(b1), None);
    assert!(!st(|s| s.bitmaps.contains_key(&b1)));
    assert_eq!(com::refs(b2), Some(2));
    // the same target again changes nothing
    assert_eq!(call(SetTarget, &[dc, b2]), 0);
    assert_eq!(com::refs(b2), Some(2));
    assert_eq!(call(DcRelease, &[dc]), 0);
    assert!(!st(|s| s.dcs.contains_key(&dc) || s.parents.contains_key(&dc)));
    assert_eq!(com::refs(b2), Some(1));
    assert_eq!(com::refs(d), Some(1));
    assert_eq!(call(BitmapRelease, &[b2]), 0);
    assert!(!st(|s| s.bitmaps.contains_key(&b2)));
    assert_eq!(com::release_obj(d), 0);
    assert!(!st(|s| s.parents.contains_key(&d)));
    assert_eq!(com::refs(f), Some(1));
    assert_eq!(com::release_obj(f), 0);
}

/// D2D1CreateDevice's factory is the device's alone: it goes with the device.
#[test]
fn a_device_of_its_own_factory_takes_it_along() {
    let _g = enter();
    let d = made(D2D1CreateDevice, &[0, 0]);
    let f = st(|s| s.parents.get(&d).copied()).unwrap();
    assert_eq!(com::refs(f), Some(1));
    assert_eq!(com::release_obj(d), 0);
    assert_eq!(com::refs(f), None);
}

#[test]
fn geometries_hold_their_source_and_sinks_their_geometry() {
    let _g = enter();
    let f = factory();
    let pg = made(CreatePathGeometry, &[f]);
    let sink = made(GeomOpen, &[pg]);
    let m = floats(&[1.0, 0.0, 0.0, 1.0, 5.0, 5.0]);
    let tg = made(CreateTransformedGeometry, &[f, pg, m]);
    mem::heap_free(m);
    assert_eq!(com::refs(f), Some(3));
    assert_eq!(com::release_obj(pg), 2);
    assert_eq!(com::release_obj(sink), 0);
    assert_eq!(com::refs(pg), Some(1));
    assert_eq!(com::release_obj(tg), 0);
    assert_eq!((com::refs(pg), com::refs(tg)), (None, None));
    assert!(geoms(|g| !g.contains_key(&pg) && !g.contains_key(&tg)));
    assert_eq!(com::refs(f), Some(1));
    assert_eq!(com::release_obj(f), 0);
}

proptest! {
    /// A chain of transformed geometries over one path, released in any order (each object
    /// once by the guest): everything goes, the factory is left with its own reference.
    #[test]
    fn geometry_chains_go_in_any_release_order(n in 1usize..6, order in proptest::collection::vec(any::<prop::sample::Index>(), 8)) {
        let _g = enter();
        let f = factory();
        let mut objs = vec![made(CreatePathGeometry, &[f])];
        objs.push(made(GeomOpen, &[objs[0]]));
        let m = floats(&[2.0, 0.0, 0.0, 2.0, 0.0, 0.0]);
        for i in 0..n {
            let src = if i == 0 { objs[0] } else { objs[objs.len() - 1] };
            objs.push(made(CreateTransformedGeometry, &[f, src, m]));
        }
        mem::heap_free(m);
        let mut left = objs.clone();
        for k in order {
            if left.is_empty() { break; }
            let o = left.remove(k.index(left.len()));
            com::release_obj(o);
        }
        for o in left {
            com::release_obj(o);
        }
        for &o in &objs {
            prop_assert_eq!(com::refs(o), None);
            prop_assert!(geoms(|g| !g.contains_key(&o)));
        }
        prop_assert_eq!(com::refs(f), Some(1));
        com::release_obj(f);
    }
}
