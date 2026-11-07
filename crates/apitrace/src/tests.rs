//! The reader against a writer that writes as apitrace's does, and the converter on a D3D9
//! trace shaped as apitrace's d3d9 wrapper records one (specs/d3d9.py, wrappers/d3d9trace.py).

use crate::replay::{self, Arg, Record};
use crate::trace::writer::Writer;
use crate::trace::*;
use proptest::prelude::*;
use std::rc::Rc;

fn f(name: &str, args: &[&str]) -> Rc<FunctionSig> {
    Rc::new(FunctionSig { name: name.into(), args: args.iter().map(|s| s.to_string()).collect() })
}

fn st(name: &str, members: &[&str], v: Vec<Value>) -> Value {
    Value::Struct(Rc::new(StructSig { name: name.into(), members: members.iter().map(|s| s.to_string()).collect() }), v)
}

fn parse_all(file: &[u8]) -> Vec<Call> {
    let ev = unpack(file).unwrap();
    let mut p = Parser::new(&ev).unwrap();
    let mut v = vec![];
    while let Some(c) = p.next_call().unwrap() {
        v.push(c);
    }
    v
}

/// apitrace writes a non-negative signed integer as UINT; floats compared by their bits (NaN)
fn norm(v: &Value) -> Value {
    match v {
        Value::Float(x) => Value::Str(format!("f{}", x.to_bits()).into_bytes()),
        Value::Double(x) => Value::Str(format!("d{}", x.to_bits()).into_bytes()),
        Value::SInt(x) if *x >= 0 => Value::UInt(*x as u64),
        Value::Array(a) => Value::Array(a.iter().map(norm).collect()),
        Value::Struct(s, m) => Value::Struct(s.clone(), m.iter().map(norm).collect()),
        Value::Repr(h, m) => Value::Repr(Box::new(norm(h)), Box::new(norm(m))),
        v => v.clone(),
    }
}

fn value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|x| Value::SInt(x.max(i64::MIN + 1))),
        any::<u64>().prop_map(Value::UInt),
        any::<u32>().prop_map(|b| Value::Float(f32::from_bits(b))),
        any::<u64>().prop_map(|b| Value::Double(f64::from_bits(b))),
        proptest::collection::vec(any::<u8>(), 0..20).prop_map(Value::Str),
        proptest::collection::vec(any::<u8>(), 0..40).prop_map(Value::Blob),
        any::<u64>().prop_map(|a| Value::Opaque(a.max(1))),
        proptest::collection::vec(0u32..0x11000, 0..8).prop_map(Value::WStr),
        (0usize..3, any::<i64>()).prop_map(|(i, x)| {
            thread_local!(static S: Vec<Rc<EnumSig>> = (0..3).map(|k| Rc::new(EnumSig { values: (0..k + 1).map(|j| (format!("E{k}_{j}"), j as i64 - 1)).collect() })).collect());
            Value::Enum(S.with(|s| s[i].clone()), x.max(i64::MIN + 1))
        }),
        (0usize..2, any::<u64>()).prop_map(|(i, x)| {
            thread_local!(static S: Vec<Rc<BitmaskSig>> = (0..2).map(|k| Rc::new(BitmaskSig { flags: vec![(format!("B{k}"), 1 << k), ("X".into(), 8)] })).collect());
            Value::Bitmask(S.with(|s| s[i].clone()), x)
        }),
    ];
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Value::Repr(Box::new(a), Box::new(b))),
            proptest::collection::vec(inner, 2).prop_map(|m| {
                thread_local!(static S: Rc<StructSig> = Rc::new(StructSig { name: "S".into(), members: vec!["a".into(), "b".into()] }));
                Value::Struct(S.with(|s| s.clone()), m)
            }),
        ]
    })
}

proptest! {
    /// every value, any chunking of the file, calls ending in any order
    #[test]
    fn reads_what_was_written(calls in proptest::collection::vec((proptest::collection::vec(value(), 0..4), proptest::option::of(value()), any::<bool>(), 0u64..3), 1..12), chunk in 1usize..300, reverse in any::<bool>()) {
        let sigs: Vec<_> = (0..calls.len()).map(|i| f(&format!("F{}", i % 3), &["a", "b", "c", "d"])).collect();
        let mut w = Writer::new(&[("process.arch", "x86")]);
        let nos: Vec<u64> = calls.iter().zip(&sigs).map(|((args, _, _, t), s)| {
            let a: Vec<_> = args.iter().cloned().enumerate().collect();
            w.enter(*t, s, &a)
        }).collect();
        let order: Vec<usize> = if reverse { (0..calls.len()).rev().collect() } else { (0..calls.len()).collect() };
        for &i in &order {
            w.leave(nos[i], &[], calls[i].1.as_ref(), calls[i].2);
        }
        let got = parse_all(&w.file(chunk));
        prop_assert_eq!(got.len(), calls.len());
        for (g, &i) in got.iter().zip(&order) {
            prop_assert_eq!(g.no, nos[i]);
            prop_assert_eq!(&g.sig.name, &sigs[i].name);
            prop_assert_eq!(g.thread, calls[i].3);
            prop_assert_eq!(g.fake, calls[i].2);
            let want: Vec<_> = calls[i].0.iter().map(|v| Some(norm(v))).chain(std::iter::repeat(None)).take(4).collect();
            let got_in: Vec<_> = g.args_in.iter().map(|v| v.as_ref().map(norm)).collect();
            prop_assert_eq!(&got_in, &want);
            prop_assert_eq!(g.ret.as_ref().map(norm), calls[i].1.as_ref().map(norm));
        }
    }

    /// broken files are errors, not panics
    #[test]
    fn garbage_is_an_error(mut bytes in proptest::collection::vec(any::<u8>(), 0..300), cut in any::<prop::sample::Index>()) {
        let _ = Parser::new(&bytes).and_then(|mut p| { while p.next_call()?.is_some() {} Ok(()) });
        // a real header with garbage after it
        let mut w = Writer::new(&[]);
        w.b.append(&mut bytes);
        let n = cut.index(w.b.len() + 1);
        let _ = Parser::new(&w.b[..n]).and_then(|mut p| { while p.next_call()?.is_some() {} Ok(()) });
        let _ = replay::convert(&w.b[..n]);
        let _ = replay::read(&w.b[..n]);
    }
}

#[test]
fn properties_and_version() {
    let w = Writer::new(&[("process.arch", "x86"), ("process.name", "game.exe")]);
    let ev = unpack(&w.file(64)).unwrap();
    let p = Parser::new(&ev).unwrap();
    assert_eq!(p.version, 6);
    assert_eq!(p.properties, vec![("process.arch".into(), "x86".into()), ("process.name".into(), "game.exe".into())]);
    assert!(unpack(b"xx").is_err());
    let mut v7 = Writer::default();
    v7.uint(7);
    assert!(Parser::new(&v7.b).is_err());
}

/// A small D3D9 session as apitrace records it
fn d3d9_session() -> Vec<u8> {
    let mut w = Writer::new(&[("process.arch", "x86")]);
    let create = f("Direct3DCreate9", &["SDKVersion"]);
    let n = w.enter(0, &create, &[(0, Value::UInt(32))]);
    w.leave(n, &[], Some(&Value::Opaque(0x1000)), false);

    let pp_names = ["BackBufferWidth", "BackBufferHeight", "BackBufferFormat", "BackBufferCount", "MultiSampleType", "MultiSampleQuality", "SwapEffect", "hDeviceWindow", "Windowed", "EnableAutoDepthStencil", "AutoDepthStencilFormat", "Flags", "FullScreen_RefreshRateInHz", "PresentationInterval"];
    let mut pp: Vec<Value> = (0..14).map(|i| Value::UInt(i)).collect();
    pp[7] = Value::Opaque(0x777);
    let pp = st("D3DPRESENT_PARAMETERS", &pp_names, pp);
    let cd = f("IDirect3D9::CreateDevice", &["this", "Adapter", "DeviceType", "hFocusWindow", "BehaviorFlags", "pPresentationParameters", "ppReturnedDeviceInterface"]);
    let n = w.enter(0, &cd, &[(0, Value::Opaque(0x1000)), (1, Value::UInt(0)), (2, Value::UInt(1)), (3, Value::Opaque(0x777)), (4, Value::UInt(0x20))]);
    w.leave(n, &[(5, Value::Array(vec![pp])), (6, Value::Array(vec![Value::Opaque(0x2000)]))], Some(&Value::UInt(0)), false);

    // a query: left out
    let caps = f("IDirect3DDevice9::GetDeviceCaps", &["this", "pCaps"]);
    let n = w.enter(0, &caps, &[(0, Value::Opaque(0x2000))]);
    w.leave(n, &[(1, Value::Array(vec![Value::Blob(vec![1; 8])]))], Some(&Value::UInt(0)), false);

    let ct = f("IDirect3DDevice9::CreateTexture", &["this", "Width", "Height", "Levels", "Usage", "Format", "Pool", "ppTexture", "pSharedHandle"]);
    let n = w.enter(0, &ct, &[(0, Value::Opaque(0x2000)), (1, Value::UInt(4)), (2, Value::UInt(2)), (3, Value::UInt(1)), (4, Value::UInt(0)), (5, Value::UInt(21)), (6, Value::UInt(1))]);
    w.leave(n, &[(7, Value::Array(vec![Value::Opaque(0x3000)])), (8, Value::Null)], Some(&Value::UInt(0)), false);

    let n = w.enter(0, &ct, &[(0, Value::Opaque(0x2000)), (1, Value::UInt(1)), (2, Value::UInt(1)), (3, Value::UInt(1)), (4, Value::UInt(0)), (5, Value::UInt(62)), (6, Value::UInt(1))]);
    w.leave(n, &[], Some(&Value::SInt(-2005530516)), false);
    let gds = f("IDirect3DDevice9::GetDepthStencilSurface", &["this", "ppZStencilSurface"]);
    let n = w.enter(0, &gds, &[(0, Value::Opaque(0x2000))]);
    w.leave(n, &[(1, Value::Null)], Some(&Value::SInt(-2005530522)), false);

    let lr = f("IDirect3DTexture9::LockRect", &["this", "Level", "pLockedRect", "pRect", "Flags"]);
    let n = w.enter(0, &lr, &[(0, Value::Opaque(0x3000)), (1, Value::UInt(0)), (3, Value::Null), (4, Value::UInt(0))]);
    let locked = st("D3DLOCKED_RECT", &["Pitch", "pBits"], vec![Value::UInt(16), Value::Opaque(0x9000)]);
    w.leave(n, &[(2, Value::Array(vec![locked]))], Some(&Value::UInt(0)), false);
    // (UnlockRect: apitrace's memcpy of the locked bytes first)
    let mc = f("memcpy", &["dest", "src", "n"]);
    let n = w.enter(0, &mc, &[(0, Value::Opaque(0x9000)), (1, Value::Blob((0..32).collect())), (2, Value::UInt(32))]);
    w.leave(n, &[], None, true);
    let ur = f("IDirect3DTexture9::UnlockRect", &["this", "Level"]);
    let n = w.enter(0, &ur, &[(0, Value::Opaque(0x3000)), (1, Value::UInt(0))]);
    w.leave(n, &[], Some(&Value::UInt(0)), false);

    let el = |s: u64, o: u64, t: u64, u: u64| st("D3DVERTEXELEMENT9", &["Stream", "Offset", "Type", "Method", "Usage", "UsageIndex"], vec![Value::UInt(s), Value::UInt(o), Value::UInt(t), Value::UInt(0), Value::UInt(u), Value::UInt(0)]);
    let cvd = f("IDirect3DDevice9::CreateVertexDeclaration", &["this", "pVertexElements", "ppDecl"]);
    let n = w.enter(0, &cvd, &[(0, Value::Opaque(0x2000)), (1, Value::Array(vec![el(0, 0, 2, 0), el(0, 12, 4, 10)]))]);
    w.leave(n, &[(2, Value::Array(vec![Value::Opaque(0x4000)]))], Some(&Value::UInt(0)), false);

    let srs = f("IDirect3DDevice9::SetRenderState", &["this", "State", "Value"]);
    let e = Rc::new(EnumSig { values: vec![("D3DRS_ZENABLE".into(), 7)] });
    let n = w.enter(0, &srs, &[(0, Value::Opaque(0x2000)), (1, Value::Enum(e, 7)), (2, Value::UInt(0))]);
    w.leave(n, &[], Some(&Value::UInt(0)), false);

    let st_ = f("IDirect3DDevice9::SetTexture", &["this", "Stage", "pTexture"]);
    let n = w.enter(0, &st_, &[(0, Value::Opaque(0x2000)), (1, Value::UInt(0)), (2, Value::Opaque(0x3000))]);
    w.leave(n, &[], Some(&Value::UInt(0)), false);

    let clear = f("IDirect3DDevice9::Clear", &["this", "Count", "pRects", "Flags", "Color", "Z", "Stencil"]);
    let rect = st("D3DRECT", &["x1", "y1", "x2", "y2"], vec![Value::UInt(1), Value::UInt(2), Value::UInt(3), Value::SInt(-4)]);
    let n = w.enter(0, &clear, &[(0, Value::Opaque(0x2000)), (1, Value::UInt(1)), (2, Value::Array(vec![rect])), (3, Value::UInt(1)), (4, Value::UInt(0xff00ff00)), (5, Value::Float(1.0)), (6, Value::UInt(0))]);
    w.leave(n, &[], Some(&Value::UInt(0)), false);

    let up = f("IDirect3DDevice9::DrawPrimitiveUP", &["this", "PrimitiveType", "PrimitiveCount", "pVertexStreamZeroData", "VertexStreamZeroStride"]);
    let n = w.enter(0, &up, &[(0, Value::Opaque(0x2000)), (1, Value::UInt(5)), (2, Value::UInt(2)), (3, Value::Blob(vec![9; 64])), (4, Value::UInt(16))]);
    w.leave(n, &[], Some(&Value::UInt(0)), false);

    let cvs = f("IDirect3DDevice9::CreateVertexShader", &["this", "pFunction", "ppShader"]);
    let n = w.enter(0, &cvs, &[(0, Value::Opaque(0x2000)), (1, Value::Repr(Box::new(Value::Str(b"vs_2_0\n".to_vec())), Box::new(Value::Blob(vec![0, 2, 0xfe, 0xff, 0xff, 0xff, 0, 0]))))]);
    w.leave(n, &[(2, Value::Array(vec![Value::Opaque(0x5000)]))], Some(&Value::SInt(-2147467259)), false);

    let present = f("IDirect3DDevice9::Present", &["this", "pSourceRect", "pDestRect", "hDestWindowOverride", "pDirtyRegion"]);
    let n = w.enter(0, &present, &[(0, Value::Opaque(0x2000)), (1, Value::Null), (2, Value::Null), (3, Value::Opaque(0x777)), (4, Value::Null)]);
    w.leave(n, &[], Some(&Value::UInt(0)), false);

    let perf = f("D3DPERF_BeginEvent", &["col", "wszName"]);
    let n = w.enter(0, &perf, &[(0, Value::UInt(0)), (1, Value::WStr(vec![65]))]);
    w.leave(n, &[], Some(&Value::UInt(0)), false);
    w.file(100)
}

#[test]
fn a_d3d9_session_converts() {
    let (out, stats) = replay::convert(&unpack(&d3d9_session()).unwrap()).unwrap();
    assert_eq!(stats.calls, 17);
    assert_eq!(stats.memcpys, 1);
    assert_eq!(stats.skipped.keys().collect::<Vec<_>>(), ["D3DPERF_BeginEvent", "IDirect3DDevice9::GetDeviceCaps"]);
    assert!(stats.dropped.is_empty());
    let r = replay::read(&out).unwrap();
    let call = |i: usize| match &r[i] {
        Record::Call { name, ret, args, .. } => (name.as_str(), *ret, args.clone()),
        m => panic!("{m:?}"),
    };
    // handles: d3d 1, device 2, texture 3, declaration 4, shader 5
    assert_eq!(call(0), ("Direct3DCreate9", None, vec![Arg::Word(32), Arg::OutObj(1)]));
    let (n, ret, a) = call(1);
    assert_eq!((n, ret), ("IDirect3D9::CreateDevice", Some(0)));
    assert_eq!(a[..5], [Arg::Handle(1), Arg::Word(0), Arg::Word(1), Arg::Window, Arg::Word(0x20)]);
    let Arg::Blob(pp) = &a[5] else { panic!() };
    assert_eq!(pp.len(), 14 * 4);
    assert_eq!(pp[7 * 4..8 * 4], 0x777u32.to_le_bytes());
    assert_eq!(pp[13 * 4..], 13u32.to_le_bytes());
    assert_eq!(a[6], Arg::OutObj(2));
    assert_eq!(call(2).2, vec![Arg::Handle(2), Arg::Word(4), Arg::Word(2), Arg::Word(1), Arg::Word(0), Arg::Word(21), Arg::Word(1), Arg::OutObj(3), Arg::Word(0)]);
    assert_eq!(call(3).2[7..], [Arg::OutObj(0), Arg::Word(0)]);
    assert_eq!(call(4).2, vec![Arg::Handle(2), Arg::OutObj(0)]);
    let r: Vec<_> = r.into_iter().enumerate().filter(|(i, _)| !matches!(i, 3 | 4)).map(|(_, x)| x).collect();
    let call = |i: usize| match &r[i] {
        Record::Call { name, ret, args, .. } => (name.as_str(), *ret, args.clone()),
        m => panic!("{m:?}"),
    };
    assert_eq!(call(3).2, vec![Arg::Handle(3), Arg::Word(0), Arg::Lock(replay::LOCKRECT, 0), Arg::Word(0), Arg::Word(0)]);
    assert_eq!(r[4], Record::Memcpy { handle: 3, key: 0, offset: 0, pitch: 16, slice: 0, data: (0..32).collect() });
    assert_eq!(call(5).0, "IDirect3DTexture9::UnlockRect");
    // two 8-byte elements and the D3DDECL_END apitrace left out
    let (_, _, a) = call(6);
    assert_eq!(a[1], Arg::Blob(vec![0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 12, 0, 4, 0, 10, 0, 0xff, 0, 0, 0, 17, 0, 0, 0]));
    assert_eq!(a[2], Arg::OutObj(4));
    assert_eq!(call(7).2, vec![Arg::Handle(2), Arg::Word(7), Arg::Word(0)]);
    assert_eq!(call(8).2, vec![Arg::Handle(2), Arg::Word(0), Arg::Handle(3)]);
    let (_, _, a) = call(9);
    assert_eq!(a[2], Arg::Blob([1u32, 2, 3, (-4i32) as u32].iter().flat_map(|w| w.to_le_bytes()).collect()));
    assert_eq!(a[5], Arg::Word(1.0f32.to_bits()));
    assert_eq!(call(10).2[3], Arg::Blob(vec![9; 64]));
    // a shader is its tokens; a failed call's return value is kept
    let (_, ret, a) = call(11);
    assert_eq!(ret, Some(0x80004005));
    assert_eq!(a[1], Arg::Blob(vec![0, 2, 0xfe, 0xff, 0xff, 0xff, 0, 0]));
    assert_eq!(call(12).2, vec![Arg::Handle(2), Arg::Word(0), Arg::Word(0), Arg::Window, Arg::Word(0)]);
    assert_eq!(r.len(), 13);
}

#[test]
fn locks_of_buffers_volumes_and_cube_faces() {
    let mut w = Writer::new(&[]);
    let lock = f("IDirect3DVertexBuffer9::Lock", &["this", "OffsetToLock", "SizeToLock", "ppbData", "Flags"]);
    let n = w.enter(0, &lock, &[(0, Value::Opaque(0x100)), (1, Value::UInt(16)), (2, Value::UInt(8)), (4, Value::UInt(0x2000))]);
    w.leave(n, &[(3, Value::Array(vec![Value::Opaque(0x8010)]))], Some(&Value::UInt(0)), false);
    let cube = f("IDirect3DCubeTexture9::LockRect", &["this", "FaceType", "Level", "pLockedRect", "pRect", "Flags"]);
    let n = w.enter(0, &cube, &[(0, Value::Opaque(0x200)), (1, Value::UInt(3)), (2, Value::UInt(2)), (4, Value::Null), (5, Value::UInt(0))]);
    w.leave(n, &[(3, Value::Array(vec![st("D3DLOCKED_RECT", &["Pitch", "pBits"], vec![Value::UInt(8), Value::Opaque(0xa000)])]))], Some(&Value::UInt(0)), false);
    let vol = f("IDirect3DVolumeTexture9::LockBox", &["this", "Level", "pLockedVolume", "pBox", "Flags"]);
    let n = w.enter(0, &vol, &[(0, Value::Opaque(0x300)), (1, Value::UInt(1)), (3, Value::Null), (4, Value::UInt(0))]);
    w.leave(n, &[(2, Value::Array(vec![st("D3DLOCKED_BOX", &["RowPitch", "SlicePitch", "pBits"], vec![Value::UInt(8), Value::UInt(32), Value::Opaque(0xb000)])]))], Some(&Value::UInt(0)), false);
    let mc = f("memcpy", &["dest", "src", "n"]);
    for (dest, len) in [(0x8010u64, 8usize), (0xa000, 16), (0xb000, 64), (0xb008, 4), (0x10, 1)] {
        let n = w.enter(0, &mc, &[(0, Value::Opaque(dest)), (1, Value::Blob(vec![1; len])), (2, Value::UInt(len as u64))]);
        w.leave(n, &[], None, true);
    }
    let (out, stats) = replay::convert(&w.b).unwrap();
    assert_eq!(stats.dropped.get("memcpy (into no lock)"), Some(&1));
    let r = replay::read(&out).unwrap();
    let m: Vec<_> = r.iter().filter_map(|x| match x { Record::Memcpy { handle, key, offset, pitch, slice, data } => Some((*handle, *key, *offset, *pitch, *slice, data.len())), _ => None }).collect();
    assert_eq!(m, vec![(1, 0, 0, 0, 0, 8), (2, 3 + 2 * 6, 0, 8, 0, 16), (3, 1, 0, 8, 32, 64), (3, 1, 8, 8, 32, 4)]);
    let Record::Call { args, .. } = &r[0] else { panic!() };
    assert_eq!(args[3], Arg::Lock(replay::LOCKBUF, 0));
    let Record::Call { args, .. } = &r[2] else { panic!() };
    assert_eq!(args[2], Arg::Lock(replay::LOCKBOX, 1));
}

#[test]
fn x64_traces_are_refused() {
    let w = Writer::new(&[("process.arch", "x86_64")]);
    assert!(replay::convert(&w.b).is_err());
}

#[test]
fn struct_packing_follows_c_layout() {
    let mut b = vec![];
    let guid = st("GUID", &["Data1", "Data2", "Data3", "Data4"], vec![Value::UInt(0x11223344), Value::UInt(0x5566), Value::UInt(0x7788), Value::Array((1..=8).map(Value::UInt).collect())]);
    replay::pack(&mut b, &guid, 4).unwrap();
    assert_eq!(b, [0x44, 0x33, 0x22, 0x11, 0x66, 0x55, 0x88, 0x77, 1, 2, 3, 4, 5, 6, 7, 8]);
    let mut b = vec![];
    let m = st("D3DMATRIX", &["m"], vec![Value::Array((0..4).map(|r| Value::Array((0..4).map(|c| Value::Float((r * 4 + c) as f32)).collect())).collect())]);
    replay::pack(&mut b, &m, 4).unwrap();
    assert_eq!(b.len(), 64);
    assert_eq!(b[60..], 15.0f32.to_le_bytes());
}
