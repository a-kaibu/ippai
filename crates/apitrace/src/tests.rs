//! The reader against a writer that writes as apitrace's does; the converter on sessions shaped
//! as apitrace's wrappers record them (specs/d3d9.py, d3d8.py, d3d11.py), 32- and 64-bit, on the
//! headers' layouts, and on the real traces of tests/replay/traces (recorded on Windows).

use crate::layout::{Layouts, Ty};
use crate::replay::{self, Arg, Blob, Packer, Record};
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
        let _ = replay::convert(&w.b[..n], None);
        let _ = replay::convert(&w.b[..n], Some(64));
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
    let mut pp: Vec<Value> = (0..14).map(Value::UInt).collect();
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

/// the calls of a stream (its NAME and WINSIZE records left out)
fn calls(out: &[u8]) -> Vec<Record> {
    replay::read(out).unwrap().1.into_iter().filter(|r| !matches!(r, Record::Name { .. } | Record::WinSize(..))).collect()
}

fn call_of(r: &Record) -> (&str, Option<u32>, Vec<Arg>) {
    match r {
        Record::Call { name, ret, args, .. } => (name.as_str(), *ret, args.clone()),
        m => panic!("{m:?}"),
    }
}

fn le32(ws: &[u32]) -> Vec<u8> {
    ws.iter().flat_map(|w| w.to_le_bytes()).collect()
}

#[test]
fn a_d3d9_session_converts() {
    for bits in [32, 64] {
        let ptr = bits as usize / 8;
        let (out, stats) = replay::convert(&unpack(&d3d9_session()).unwrap(), Some(bits)).unwrap();
        assert_eq!(stats.calls, 17);
        assert_eq!(stats.memcpys, 1);
        assert_eq!(stats.skipped.keys().collect::<Vec<_>>(), ["D3DPERF_BeginEvent", "IDirect3DDevice9::GetDepthStencilSurface", "IDirect3DDevice9::GetDeviceCaps"]);
        assert!(stats.dropped.is_empty());
        let (b, all) = replay::read(&out).unwrap();
        assert_eq!(b, bits);
        // the names carry their vtable index (mingw-w64's d3d9.h) and what they return
        let names: Vec<_> = all.iter().filter_map(|r| match r { Record::Name { name, index, flags, .. } => Some((name.as_str(), *index, *flags)), _ => None }).collect();
        assert_eq!(names[..2], [("Direct3DCreate9", -1, replay::NAME_RETOBJ), ("IDirect3D9::CreateDevice", 16, replay::NAME_HR)]);
        assert!(names.contains(&("IDirect3DDevice9::Present", 17, 1)) && names.contains(&("IDirect3DDevice9::Clear", 43, 1)) && names.contains(&("IDirect3DDevice9::DrawPrimitiveUP", 83, 1)));
        // the back buffer size before the device
        let ws = all.iter().position(|r| matches!(r, Record::WinSize(0, 1))).unwrap();
        assert!(matches!(&all[ws + 1], Record::Call { name, .. } if name == "IDirect3D9::CreateDevice"));
        let r = calls(&out);
        // handles: d3d 1, device 2, texture 3, declaration 4, shader 5
        assert_eq!(call_of(&r[0]), ("Direct3DCreate9", None, vec![Arg::Word(32), Arg::OutObj(1)]));
        let (n, ret, a) = call_of(&r[1]);
        assert_eq!((n, ret), ("IDirect3D9::CreateDevice", Some(0)));
        assert_eq!(a[..5], [Arg::Handle(1), Arg::Word(0), Arg::Word(1), Arg::Window, Arg::Word(0x20)]);
        // D3DPRESENT_PARAMETERS: windowed, in the replay's window (its HWND member is a pointer:
        // 8 bytes and aligned on x64), no refresh rate, no vsync
        let Arg::Blob(pp) = &a[5] else { panic!() };
        let hwnd = if bits == 64 { 32 } else { 28 };
        let after = |i: u32| if bits == 64 && i > 7 { 4 * i + 8 } else { 4 * i } as usize;
        assert_eq!(pp.bytes.len(), [56, 64][(bits == 64) as usize]);
        assert_eq!(pp.fix, vec![(hwnd, replay::FIX_WINDOW, 0)]);
        assert_eq!(pp.bytes[after(8)..after(8) + 4], 1u32.to_le_bytes());
        assert_eq!(pp.bytes[after(12)..after(12) + 4], 0u32.to_le_bytes());
        assert_eq!(pp.bytes[after(13)..], 0x8000_0000u32.to_le_bytes());
        assert_eq!(pp.bytes[after(11)..after(11) + 4], 11u32.to_le_bytes());
        assert_eq!(a[6], Arg::OutObj(2));
        // a texture: pSharedHandle NULL (a non-Ex device wants it so)
        assert_eq!(call_of(&r[2]).2, vec![Arg::Handle(2), Arg::Word(4), Arg::Word(2), Arg::Word(1), Arg::Word(0), Arg::Word(21), Arg::Word(1), Arg::OutObj(3), Arg::Word(0)]);
        // a failed one: a place for its object still (WARP writes NULL there)
        assert_eq!(call_of(&r[3]).2[7..], [Arg::OutObj(0), Arg::Word(0)]);
        assert_eq!(call_of(&r[4]).2, vec![Arg::Handle(3), Arg::Word(0), Arg::Lock { kind: replay::LOCK_RECT, handle: 3, key: 0 }, Arg::Word(0), Arg::Word(0)]);
        assert_eq!(r[5], Record::Memcpy { handle: 3, key: 0, offset: 0, pitch: 16, slice: 0, data: (0..32).collect() });
        assert_eq!(call_of(&r[6]).0, "IDirect3DTexture9::UnlockRect");
        // two 8-byte elements and the D3DDECL_END apitrace left out
        let (_, _, a) = call_of(&r[7]);
        assert_eq!(a[1], Arg::Blob(Blob::raw(vec![0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 12, 0, 4, 0, 10, 0, 0xff, 0, 0, 0, 17, 0, 0, 0])));
        assert_eq!(a[2], Arg::OutObj(4));
        assert_eq!(call_of(&r[8]).2, vec![Arg::Handle(2), Arg::Word(7), Arg::Word(0)]);
        assert_eq!(call_of(&r[9]).2, vec![Arg::Handle(2), Arg::Word(0), Arg::Handle(3)]);
        let (_, _, a) = call_of(&r[10]);
        assert_eq!(a[2], Arg::Blob(Blob::raw(le32(&[1, 2, 3, (-4i32) as u32]))));
        // a float by value: its bits
        assert_eq!(a[5], Arg::Word(1.0f32.to_bits() as u64));
        assert_eq!(call_of(&r[11]).2[3], Arg::Blob(Blob::raw(vec![9; 64])));
        // a shader is its tokens; a failed call's return value is kept
        let (_, ret, a) = call_of(&r[12]);
        assert_eq!(ret, Some(0x80004005));
        assert_eq!(a[1], Arg::Blob(Blob::raw(vec![0, 2, 0xfe, 0xff, 0xff, 0xff, 0, 0])));
        assert_eq!(call_of(&r[13]).2, vec![Arg::Handle(2), Arg::Word(0), Arg::Word(0), Arg::Window, Arg::Word(0)]);
        assert_eq!(r.len(), 14);
        let _ = ptr;
    }
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
    let (out, stats) = replay::convert(&w.b, None).unwrap();
    assert_eq!(stats.dropped.get("memcpy (into no lock)"), Some(&1));
    let r = calls(&out);
    let m: Vec<_> = r.iter().filter_map(|x| match x { Record::Memcpy { handle, key, offset, pitch, slice, data } => Some((*handle, *key, *offset, *pitch, *slice, data.len())), _ => None }).collect();
    assert_eq!(m, vec![(1, 0, 0, 0, 0, 8), (2, 3 + 2 * 6, 0, 8, 0, 16), (3, 1, 0, 8, 32, 64), (3, 1, 8, 8, 32, 4)]);
    assert_eq!(call_of(&r[0]).2[3], Arg::Lock { kind: replay::LOCK_BUF, handle: 1, key: 0 });
    assert_eq!(call_of(&r[2]).2[2], Arg::Lock { kind: replay::LOCK_BOX, handle: 3, key: 1 });
}

#[test]
fn the_pointer_width_comes_from_the_addresses() {
    let mut w = Writer::new(&[]);
    let create = f("Direct3DCreate9", &["SDKVersion"]);
    let n = w.enter(0, &create, &[(0, Value::UInt(32))]);
    w.leave(n, &[], Some(&Value::Opaque(0x2_0000_1000)), false);
    assert_eq!(replay::ptrbits(&w.b).unwrap(), 64);
    let (out, stats) = replay::convert(&w.b, None).unwrap();
    assert_eq!((stats.ptrbits, replay::read(&out).unwrap().0), (64, 64));
    // (a 64-bit trace is not made 32-bit: its pointers would not fit)
    assert!(replay::convert(&w.b, Some(32)).is_err());
    assert!(replay::convert(&w.b, Some(16)).is_err());
    let w = Writer::new(&[]);
    assert_eq!(replay::ptrbits(&w.b).unwrap(), 32);
}

/// what a pointer to `t` (by its layout name) packs to, for `bits`
fn pack(t: &str, v: &Value, bits: u32, objects: &[(u64, u32)]) -> Blob {
    let objects = objects.iter().copied().collect();
    let mut p = Packer::with(Layouts::get(), (bits == 64) as usize, &objects);
    p.pointee(&crate::layout::parse_ty(t).unwrap(), v).unwrap();
    p.finish()
}

#[test]
fn structs_pack_as_the_headers_lay_them_out() {
    let guid = st("GUID", &["Data1", "Data2", "Data3", "Data4"], vec![Value::UInt(0x11223344), Value::UInt(0x5566), Value::UInt(0x7788), Value::Array((1..=8).map(Value::UInt).collect())]);
    assert_eq!(pack("S:_GUID", &guid, 32, &[]).bytes, [0x44, 0x33, 0x22, 0x11, 0x66, 0x55, 0x88, 0x77, 1, 2, 3, 4, 5, 6, 7, 8]);
    // D3DMATRIX's m is in an anonymous union
    let m = st("D3DMATRIX", &["m"], vec![Value::Array((0..4).map(|r| Value::Array((0..4).map(|c| Value::Float((r * 4 + c) as f32)).collect())).collect())]);
    let b = pack("S:_D3DMATRIX", &m, 64, &[]).bytes;
    assert_eq!(b.len(), 64);
    assert_eq!(b[60..], 15.0f32.to_le_bytes());
    // input elements: each SemanticName a pointer to its string after them (8 bytes, the
    // struct 32 on x64; 4 and 28 on x86)
    let el = |n: &str, off: u64| st("D3D11_INPUT_ELEMENT_DESC", &["SemanticName", "SemanticIndex", "Format", "InputSlot", "AlignedByteOffset", "InputSlotClass", "InstanceDataStepRate"], vec![Value::Str(n.into()), Value::UInt(0), Value::UInt(16), Value::UInt(0), Value::UInt(off), Value::UInt(0), Value::UInt(0)]);
    let a = Value::Array(vec![el("POSITION", 0), el("COLOR", 8)]);
    for (bits, size) in [(32, 28), (64, 32)] {
        let b = pack("S:D3D11_INPUT_ELEMENT_DESC", &a, bits, &[]);
        let ptr_size = bits / 8;
        assert_eq!(b.fix.len(), 2);
        for (i, (name, &(off, kind, to))) in ["POSITION", "COLOR"].iter().zip(&b.fix).enumerate() {
            assert_eq!((off, kind), (i as u32 * size, replay::FIX_PTR));
            assert_eq!(&b.bytes[to as usize..to as usize + name.len() + 1], [name.as_bytes(), &[0]].concat());
        }
        assert_eq!(b.bytes[(size + ptr_size + 12) as usize..(size + ptr_size + 16) as usize], 8u32.to_le_bytes(), "AlignedByteOffset of the second ({bits}-bit)");
    }
    // a union apitrace writes as its one member, under an empty name
    let srv = Value::Struct(
        Rc::new(StructSig { name: "D3D11_SHADER_RESOURCE_VIEW_DESC".into(), members: vec!["Format".into(), "ViewDimension".into(), "".into()] }),
        vec![Value::UInt(28), Value::UInt(4), st("", &["Texture2D"], vec![st("D3D11_TEX2D_SRV", &["MostDetailedMip", "MipLevels"], vec![Value::UInt(1), Value::UInt(3)])])],
    );
    assert_eq!(pack("S:D3D11_SHADER_RESOURCE_VIEW_DESC", &srv, 64, &[]).bytes, le32(&[28, 4, 1, 3, 0, 0]));
    // inline arrays of structs, a byte member among words
    let rt = |mask: u64| st("D3D11_RENDER_TARGET_BLEND_DESC", &["BlendEnable", "SrcBlend", "DestBlend", "BlendOp", "SrcBlendAlpha", "DestBlendAlpha", "BlendOpAlpha", "RenderTargetWriteMask"], vec![Value::Bool(true), Value::UInt(5), Value::UInt(6), Value::UInt(1), Value::UInt(2), Value::UInt(1), Value::UInt(1), Value::UInt(mask)]);
    let bd = st("D3D11_BLEND_DESC", &["AlphaToCoverageEnable", "IndependentBlendEnable", "RenderTarget"], vec![Value::Bool(false), Value::Bool(true), Value::Array((0..8).map(rt).collect())]);
    let b = pack("S:D3D11_BLEND_DESC", &bd, 32, &[]).bytes;
    assert_eq!(b.len(), 264);
    for i in 0..8 {
        assert_eq!(b[8 + 32 * i..8 + 32 * i + 4], 1u32.to_le_bytes());
        assert_eq!(b[8 + 32 * i + 28..8 + 32 * i + 32], [i as u8, 0, 0, 0]);
    }
    // a pointer to bytes (pSysMem) after the struct; objects in an array of them
    let sd = st("D3D11_SUBRESOURCE_DATA", &["pSysMem", "SysMemPitch", "SysMemSlicePitch"], vec![Value::Blob(vec![7; 5]), Value::UInt(5), Value::UInt(0)]);
    let b = pack("S:D3D11_SUBRESOURCE_DATA", &sd, 64, &[]);
    assert_eq!(b.fix, vec![(0, replay::FIX_PTR, 16)]);
    assert_eq!(b.bytes[16..], [7; 5]);
    let views = Value::Array(vec![Value::Opaque(0x10), Value::Null, Value::Opaque(0x30)]);
    let b = pack("*O:ID3D11ShaderResourceView", &views, 64, &[(0x10, 4), (0x30, 9)]);
    assert_eq!((b.bytes.len(), b.fix), (24, vec![(0, replay::FIX_OBJ, 4), (16, replay::FIX_OBJ, 9)]));
    // (a struct without such a member is an error, not a guess)
    let bad = st("D3D11_BOX", &["left", "nope"], vec![Value::UInt(1), Value::UInt(2)]);
    let objects = Default::default();
    let mut p = Packer::with(Layouts::get(), 0, &objects);
    assert!(p.pointee(&Ty::Rec("D3D11_BOX".into()), &bad).is_err());
}

#[test]
fn the_layouts_are_the_headers() {
    let l = Layouts::get();
    let idx = |i: &str, m: &str| l.method(i, m).map(|m| m.index);
    // (as mingw-w64's d3d9.h, d3d8.h and d3d11.h declare them)
    assert_eq!([idx("IDirect3DDevice9", "Present"), idx("IDirect3DDevice9", "Clear"), idx("IDirect3DDevice9", "SetRenderState"), idx("IDirect3DDevice9", "DrawPrimitiveUP")], [Some(17), Some(43), Some(57), Some(83)]);
    assert_eq!(idx("IDirect3DDevice9Ex", "PresentEx"), Some(121));
    assert_eq!([idx("IDirect3DDevice8", "Present"), idx("IDirect3DDevice8", "CreateVertexShader"), idx("IDirect3DDevice8", "SetVertexShader")], [Some(15), Some(75), Some(76)]);
    // a derived interface has its bases' methods at their places
    assert_eq!(idx("ID3D11DeviceContext4", "Map"), Some(14));
    assert_eq!(idx("ID3D11Device5", "CreateBuffer"), Some(3));
    assert_eq!(idx("IDXGISwapChain4", "Present"), Some(8));
    assert_eq!(idx("ID3D11Texture2D1", "Release"), Some(2));
    assert_eq!(idx("ID3D11Device", "NoSuchMethod"), None);
    let pp = &l.records["_D3DPRESENT_PARAMETERS_"];
    assert_eq!(pp.size, [56, 64]);
    let hwnd = pp.members.iter().find(|m| m.name == "hDeviceWindow").unwrap();
    assert_eq!((hwnd.off, &hwnd.ty), ([28, 32], &Ty::Hwnd));
    assert_eq!(l.records["8._D3DPRESENT_PARAMETERS_"].size, [52, 56]);
    assert_eq!(l.functions["D3D11CreateDevice"].1.len(), 10);
    assert_eq!(l.functions["CreateDXGIFactory2"].1.len(), 3);
    assert!(Layouts::parse("R x 1 1 struct t\n 0 0 a q9\n").is_err());
    assert!(Layouts::parse(" 0 0 a i4\n").is_err());
    assert!(Layouts::parse("V I -\n x Foo i4\n").is_err());
}

/// A D3D8 session: shader handles are DWORDs the device gave, an FVF is not one
#[test]
fn d3d8_shader_handles_follow_the_device() {
    let mut w = Writer::new(&[]);
    let create = f("Direct3DCreate8", &["SDKVersion"]);
    let n = w.enter(0, &create, &[(0, Value::UInt(220))]);
    w.leave(n, &[], Some(&Value::Opaque(0x100)), false);
    let pp_names = ["BackBufferWidth", "BackBufferHeight", "BackBufferFormat", "BackBufferCount", "MultiSampleType", "SwapEffect", "hDeviceWindow", "Windowed", "EnableAutoDepthStencil", "AutoDepthStencilFormat", "Flags", "FullScreen_RefreshRateInHz", "FullScreen_PresentationInterval"];
    let mut pp: Vec<Value> = (0..13).map(|i| Value::UInt(64 + i)).collect();
    pp[6] = Value::Opaque(0x777);
    pp[12] = Value::UInt(0x8000_0000);
    let cd = f("IDirect3D8::CreateDevice", &["this", "Adapter", "DeviceType", "hFocusWindow", "BehaviorFlags", "pPresentationParameters", "ppReturnedDeviceInterface"]);
    let n = w.enter(0, &cd, &[(0, Value::Opaque(0x100)), (1, Value::UInt(0)), (2, Value::UInt(1)), (3, Value::Opaque(0x777)), (4, Value::UInt(0x20))]);
    w.leave(n, &[(5, Value::Array(vec![st("D3DPRESENT_PARAMETERS", &pp_names, pp)])), (6, Value::Array(vec![Value::Opaque(0x200)]))], Some(&Value::UInt(0)), false);
    let cvs = f("IDirect3DDevice8::CreateVertexShader", &["this", "pDeclaration", "pFunction", "pHandle", "Usage"]);
    let n = w.enter(0, &cvs, &[(0, Value::Opaque(0x200)), (1, Value::Array(vec![Value::UInt(0x2000_0000), Value::UInt(0xffff_ffff)])), (2, Value::Blob(vec![1, 1, 0xfe, 0xff])), (4, Value::UInt(0))]);
    w.leave(n, &[(3, Value::Array(vec![Value::UInt(3)]))], Some(&Value::UInt(0)), false);
    let svs = f("IDirect3DDevice8::SetVertexShader", &["this", "Handle"]);
    for h in [3u64, 0x144] {
        let n = w.enter(0, &svs, &[(0, Value::Opaque(0x200)), (1, Value::UInt(h))]);
        w.leave(n, &[], Some(&Value::UInt(0)), false);
    }
    // CopyRects of 2 rects apitrace recorded one of: one replayed
    let cr = f("IDirect3DDevice8::CopyRects", &["this", "pSourceSurface", "pSourceRectsArray", "cRects", "pDestinationSurface", "pDestPointsArray"]);
    let rect = st("RECT", &["left", "top", "right", "bottom"], vec![Value::UInt(0), Value::UInt(0), Value::UInt(8), Value::UInt(8)]);
    let pt = st("POINT", &["x", "y"], vec![Value::UInt(30), Value::UInt(2)]);
    let n = w.enter(0, &cr, &[(0, Value::Opaque(0x200)), (1, Value::Null), (2, Value::Array(vec![rect])), (3, Value::UInt(2)), (4, Value::Null), (5, Value::Array(vec![pt]))]);
    w.leave(n, &[], Some(&Value::UInt(0)), false);
    let (out, stats) = replay::convert(&w.b, None).unwrap();
    assert_eq!(stats.dropped.values().collect::<Vec<_>>(), [&1]);
    let r = calls(&out);
    let (_, _, a) = call_of(&r[5]);
    assert_eq!(a[2..4], [Arg::Blob(Blob::raw(le32(&[0, 0, 8, 8]))), Arg::Word(1)]);
    assert_eq!(a[5], Arg::Blob(Blob::raw(le32(&[30, 2]))));
    let (_, _, a) = call_of(&r[1]);
    let Arg::Blob(pp) = &a[5] else { panic!() };
    // (windowed, in the replay's window, the default interval a windowed D3D8 device wants)
    assert_eq!(pp.bytes.len(), 52);
    assert_eq!(pp.fix, vec![(24, replay::FIX_WINDOW, 0)]);
    assert_eq!(pp.bytes[28..32], 1u32.to_le_bytes());
    assert_eq!(pp.bytes[44..], le32(&[0, 0]));
    let (_, _, a) = call_of(&r[2]);
    assert_eq!(a[1], Arg::Blob(Blob::raw(le32(&[0x2000_0000, 0xffff_ffff]))));
    assert_eq!(a[2], Arg::Blob(Blob::raw(vec![1, 1, 0xfe, 0xff])));
    let Arg::OutObj(h) = a[3] else { panic!("{a:?}") };
    assert_eq!(call_of(&r[3]).2[1], Arg::Handle(h));
    assert_eq!(call_of(&r[4]).2[1], Arg::Word(0x144));
}

/// A DXGI 1.3+ start (dl_RJ365650): CreateDXGIFactory2, the adapter from
/// EnumAdapterByGpuPreference, D3D11CreateDevice on it (driver type UNKNOWN). Without the factory
/// the adapter was no object, the device got none and Windows answered E_INVALIDARG.
#[test]
fn a_dxgi_factory2_start_converts() {
    for bits in [32, 64] {
        let mut w = Writer::new(&[]);
        let base = if bits == 64 { 0x1_0000_0000u64 } else { 0 };
        let o = |a: u64| Value::Opaque(base + a);
        let guid = || st("GUID", &["Data1", "Data2", "Data3", "Data4"], vec![Value::UInt(1), Value::UInt(2), Value::UInt(3), Value::Array(vec![Value::UInt(4); 8])]);
        let cf = f("CreateDXGIFactory2", &["Flags", "riid", "ppFactory"]);
        let n = w.enter(0, &cf, &[(0, Value::UInt(1)), (1, guid())]);
        w.leave(n, &[(2, Value::Array(vec![o(0x100)]))], Some(&Value::UInt(0)), false);
        let ea = f("IDXGIFactory7::EnumAdapterByGpuPreference", &["this", "Adapter", "GpuPreference", "riid", "ppvAdapter"]);
        let n = w.enter(0, &ea, &[(0, o(0x100)), (1, Value::UInt(0)), (2, Value::UInt(2)), (3, guid())]);
        w.leave(n, &[(4, Value::Array(vec![o(0x200)]))], Some(&Value::UInt(0)), false);
        let cd = f("D3D11CreateDevice", &["pAdapter", "DriverType", "Software", "Flags", "pFeatureLevels", "FeatureLevels", "SDKVersion", "ppDevice", "pFeatureLevel", "ppImmediateContext"]);
        let n = w.enter(0, &cd, &[(0, o(0x200)), (1, Value::UInt(0)), (2, Value::Null), (3, Value::UInt(0x20)), (4, Value::Null), (5, Value::UInt(0)), (6, Value::UInt(7))]);
        w.leave(n, &[(7, Value::Array(vec![o(0x300)])), (8, Value::Null), (9, Value::Array(vec![o(0x400)]))], Some(&Value::UInt(0)), false);
        let (out, stats) = replay::convert(&w.b, None).unwrap();
        assert_eq!(stats.ptrbits, bits);
        assert!(stats.dropped.is_empty(), "{stats:?}");
        let r = calls(&out);
        assert_eq!(r.len(), 3, "{r:?}");
        let (name, _, a) = call_of(&r[0]);
        assert_eq!((name, a[0].clone(), a[2].clone()), ("CreateDXGIFactory2", Arg::Word(1), Arg::OutObj(1)));
        assert_eq!(call_of(&r[1]).2[0], Arg::Handle(1));
        assert_eq!(call_of(&r[1]).2[4], Arg::OutObj(2));
        // (the adapter is the factory's object 2: the device is made on it)
        assert_eq!(call_of(&r[2]).2[..2], [Arg::Handle(2), Arg::Word(0)]);
    }
}

/// A D3D11 session: Map / memcpy / Unmap of a buffer and a texture, views bound by arrays of
/// objects, objects handed out by a Get (OMGetRenderTargets), a Get without any left out
#[test]
fn a_d3d11_session_converts() {
    for bits in [32, 64] {
        let mut w = Writer::new(&[]);
        let base = if bits == 64 { 0x1_0000_0000u64 } else { 0 };
        let o = |a: u64| Value::Opaque(base + a);
        let cd = f("D3D11CreateDevice", &["pAdapter", "DriverType", "Software", "Flags", "pFeatureLevels", "FeatureLevels", "SDKVersion", "ppDevice", "pFeatureLevel", "ppImmediateContext"]);
        let n = w.enter(0, &cd, &[(0, Value::Null), (1, Value::UInt(1)), (2, Value::Null), (3, Value::UInt(0)), (4, Value::Array(vec![Value::UInt(0xb000)])), (5, Value::UInt(1)), (6, Value::UInt(7))]);
        w.leave(n, &[(7, Value::Array(vec![o(0x100)])), (8, Value::Null), (9, Value::Array(vec![o(0x200)]))], Some(&Value::UInt(0)), false);
        let bdesc = st("D3D11_BUFFER_DESC", &["ByteWidth", "Usage", "BindFlags", "CPUAccessFlags", "MiscFlags", "StructureByteStride"], vec![Value::UInt(64), Value::UInt(2), Value::UInt(4), Value::UInt(0x10000), Value::UInt(0), Value::UInt(0)]);
        let cb = f("ID3D11Device5::CreateBuffer", &["this", "pDesc", "pInitialData", "ppBuffer"]);
        let n = w.enter(0, &cb, &[(0, o(0x100)), (1, Value::Array(vec![bdesc])), (2, Value::Null)]);
        w.leave(n, &[(3, Value::Array(vec![o(0x300)]))], Some(&Value::UInt(0)), false);
        let map = f("ID3D11DeviceContext4::Map", &["this", "pResource", "Subresource", "MapType", "MapFlags", "pMappedResource"]);
        let mapped = |p: u64, pitch: u64| Value::Array(vec![st("D3D11_MAPPED_SUBRESOURCE", &["pData", "RowPitch", "DepthPitch"], vec![o(p), Value::UInt(pitch), Value::UInt(pitch * 4)])]);
        let n = w.enter(0, &map, &[(0, o(0x200)), (1, o(0x300)), (2, Value::UInt(0)), (3, Value::UInt(4)), (4, Value::UInt(0))]);
        w.leave(n, &[(5, mapped(0x9000, 64))], Some(&Value::UInt(0)), false);
        let mc = f("memcpy", &["dest", "src", "n"]);
        let n = w.enter(0, &mc, &[(0, o(0x9000)), (1, Value::Blob(vec![5; 64])), (2, Value::UInt(64))]);
        w.leave(n, &[], None, true);
        // a texture's level 1 (pitches kept: the replay's may differ)
        let n = w.enter(0, &map, &[(0, o(0x200)), (1, o(0x400)), (2, Value::UInt(1)), (3, Value::UInt(4)), (4, Value::UInt(0))]);
        w.leave(n, &[(5, mapped(0xa000, 32))], Some(&Value::UInt(0)), false);
        let n = w.enter(0, &mc, &[(0, o(0xa040)), (1, Value::Blob(vec![6; 8])), (2, Value::UInt(8))]);
        w.leave(n, &[], None, true);
        let omrt = f("ID3D11DeviceContext4::OMSetRenderTargets", &["this", "NumViews", "ppRenderTargetViews", "pDepthStencilView"]);
        let n = w.enter(0, &omrt, &[(0, o(0x200)), (1, Value::UInt(2)), (2, Value::Array(vec![o(0x300), Value::Null])), (3, Value::Null)]);
        w.leave(n, &[], None, false);
        let get = f("ID3D11DeviceContext4::OMGetRenderTargets", &["this", "NumViews", "ppRenderTargetViews", "ppDepthStencilView"]);
        let n = w.enter(0, &get, &[(0, o(0x200)), (1, Value::UInt(2))]);
        w.leave(n, &[(2, Value::Array(vec![o(0x300), Value::Null])), (3, Value::Array(vec![Value::Null]))], None, false);
        let n = w.enter(0, &get, &[(0, o(0x200)), (1, Value::UInt(1))]);
        w.leave(n, &[(2, Value::Array(vec![Value::Null])), (3, Value::Null)], None, false);
        let clear = f("ID3D11DeviceContext4::ClearRenderTargetView", &["this", "pRenderTargetView", "ColorRGBA"]);
        let n = w.enter(0, &clear, &[(0, o(0x200)), (1, o(0x300)), (2, Value::Array(vec![Value::Float(0.5), Value::Float(1.0), Value::Float(0.0), Value::Float(0.25)]))]);
        w.leave(n, &[], None, false);
        let (out, stats) = replay::convert(&w.b, None).unwrap();
        assert_eq!(stats.ptrbits, bits);
        assert_eq!(stats.skipped.keys().collect::<Vec<_>>(), ["ID3D11DeviceContext4::OMGetRenderTargets"], "{stats:?} {:?}", calls(&out));
        let r = calls(&out);
        // device 1, context 2, buffer 3
        assert_eq!(call_of(&r[0]), ("D3D11CreateDevice", Some(0), vec![Arg::Word(0), Arg::Word(1), Arg::Word(0), Arg::Word(0), Arg::Blob(Blob::raw(le32(&[0xb000]))), Arg::Word(1), Arg::Word(7), Arg::OutObj(1), Arg::Word(0), Arg::OutObj(2)]));
        assert_eq!(call_of(&r[1]).2[1], Arg::Blob(Blob::raw(le32(&[64, 2, 4, 0x10000, 0, 0]))));
        assert_eq!(call_of(&r[2]).2, vec![Arg::Handle(2), Arg::Handle(3), Arg::Word(0), Arg::Word(4), Arg::Word(0), Arg::Lock { kind: replay::LOCK_MAP, handle: 3, key: 0 }]);
        // (a buffer is linear)
        assert_eq!(r[3], Record::Memcpy { handle: 3, key: 0, offset: 0, pitch: 0, slice: 0, data: vec![5; 64] });
        assert_eq!(call_of(&r[4]).2[5], Arg::Lock { kind: replay::LOCK_MAP, handle: 4, key: 1 });
        assert_eq!(r[5], Record::Memcpy { handle: 4, key: 1, offset: 0x40, pitch: 32, slice: 128, data: vec![6; 8] });
        let ptr = bits / 8;
        assert_eq!(call_of(&r[6]).2[2], Arg::Blob(Blob { bytes: vec![0; 2 * ptr as usize], fix: vec![(0, replay::FIX_OBJ, 3)] }));
        assert_eq!(call_of(&r[7]).2[2..], [Arg::OutArr(vec![3, 0]), Arg::OutObj(0)]);
        assert_eq!(call_of(&r[8]).2[2], Arg::Blob(Blob::raw([0.5f32, 1.0, 0.0, 0.25].iter().flat_map(|f| f.to_le_bytes()).collect())));
        assert_eq!(r.len(), 9);
    }
}

/// every pointer a blob holds lands inside it, every fixed-up word fits, none overlap
fn blob_ok(b: &Blob, ptr: u32) -> Result<(), String> {
    let mut spans: Vec<(u32, u32)> = vec![];
    for &(off, kind, v) in &b.fix {
        if off + ptr > b.bytes.len() as u32 {
            return Err(format!("fixup at {off} past {}", b.bytes.len()));
        }
        if kind == replay::FIX_PTR && v > b.bytes.len() as u32 {
            return Err(format!("pointer to {v} past {}", b.bytes.len()));
        }
        if kind > replay::FIX_WINDOW {
            return Err(format!("fixup kind {kind}"));
        }
        if spans.iter().any(|&(a, e)| off < e && a < off + ptr) {
            return Err(format!("fixups overlap at {off}"));
        }
        spans.push((off, off + ptr));
    }
    Ok(())
}

/// The traces recorded on Windows (tests/replay/traces): each converts at its width, what the
/// converter drops is only what it knows it drops, every argument is sound
#[test]
fn the_recorded_traces_convert() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/replay/traces");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_none_or(|x| x != "trace") {
            continue;
        }
        let name = p.file_stem().unwrap().to_str().unwrap().to_string();
        let bits: u32 = name.rsplit('-').next().unwrap().parse().unwrap();
        let ev = unpack(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(replay::ptrbits(&ev).unwrap(), bits, "{name}");
        let (out, stats) = replay::convert(&ev, None).unwrap_or_else(|e| panic!("{name}: {e}"));
        // (apitrace's d3d8 spec keeps one of CopyRects' rects: the d3d8 probe copies two)
        let known: Vec<_> = stats.dropped.keys().filter(|k| *k != "IDirect3DDevice8::CopyRects (rects apitrace did not record)").collect();
        assert!(known.is_empty(), "{name}: dropped {:?}", stats.dropped);
        let (b, recs) = replay::read(&out).unwrap();
        assert_eq!(b, bits);
        let mut handles = std::collections::HashSet::new();
        for r in &recs {
            if let Record::Call { name: cn, args, .. } = r {
                for a in args {
                    match a {
                        Arg::Blob(bl) => blob_ok(bl, bits / 8).unwrap_or_else(|e| panic!("{name} {cn}: {e}")),
                        Arg::OutObj(h) => {
                            handles.insert(*h);
                        }
                        Arg::OutArr(hs) => handles.extend(hs),
                        Arg::Handle(h) => assert!(handles.contains(h) || cn.starts_with("Direct3DCreate") || cn.contains("::"), "{name} {cn}: handle {h} before its object"),
                        _ => {}
                    }
                }
            }
        }
        // (and the same stream again: the converter is deterministic)
        assert_eq!(replay::convert(&ev, None).unwrap().0, out, "{name}");
        n += 1;
    }
    assert!(n >= 10, "{n} traces");
}

proptest! {
    /// any value apitrace could record for a struct packs or fails, never panics, and what it
    /// packs is sound; the struct's own size comes first
    #[test]
    fn packing_any_value_is_sound(v in value(), rec in prop::sample::select(vec!["_D3DPRESENT_PARAMETERS_", "D3D11_INPUT_ELEMENT_DESC", "D3D11_SUBRESOURCE_DATA", "D3D11_BLEND_DESC", "_D3DMATRIX", "DXGI_SWAP_CHAIN_DESC", "_D3DVERTEXELEMENT9", "8._D3DPRESENT_PARAMETERS_"]), bits in prop::sample::select(vec![32u32, 64])) {
        let objects = [(1u64, 1u32)].into_iter().collect();
        let mut p = Packer::with(Layouts::get(), (bits == 64) as usize, &objects);
        if p.pointee(&Ty::Rec(rec.into()), &v).is_ok() {
            let b = p.finish();
            prop_assert!(blob_ok(&b, bits / 8).is_ok());
        }
    }

    /// integers of a struct come back from where the layout says, at every width
    #[test]
    fn struct_members_land_at_their_offsets(vals in proptest::collection::vec(any::<u32>(), 8), bits in prop::sample::select(vec![32u32, 64])) {
        let l = Layouts::get();
        let arch = (bits == 64) as usize;
        let names = ["ByteWidth", "Usage", "BindFlags", "CPUAccessFlags", "MiscFlags", "StructureByteStride"];
        let v = st("D3D11_BUFFER_DESC", &names, vals[..6].iter().map(|&x| Value::UInt(x as u64)).collect());
        let b = pack("S:D3D11_BUFFER_DESC", &v, bits, &[]);
        let r = &l.records["D3D11_BUFFER_DESC"];
        prop_assert_eq!(b.bytes.len() as u32, r.size[arch]);
        for (n, x) in names.iter().zip(&vals) {
            let m = r.members.iter().find(|m| m.name == *n).unwrap();
            let o = m.off[arch] as usize;
            prop_assert_eq!(&b.bytes[o..o + 4], &x.to_le_bytes());
        }
        // a pointer member in the middle: the members after it move on x64
        let names = ["BufferDesc", "SampleDesc", "BufferUsage", "BufferCount", "OutputWindow", "Windowed", "SwapEffect", "Flags"];
        let mode = st("DXGI_MODE_DESC", &["Width", "Height"], vec![Value::UInt(vals[0] as u64), Value::UInt(vals[1] as u64)]);
        let sample = st("DXGI_SAMPLE_DESC", &["Count", "Quality"], vec![Value::UInt(1), Value::UInt(0)]);
        let v = st("DXGI_SWAP_CHAIN_DESC", &names, vec![mode, sample, Value::UInt(vals[2] as u64), Value::UInt(vals[3] as u64), Value::Opaque(0x55), Value::UInt(vals[4] as u64), Value::UInt(vals[5] as u64), Value::UInt(vals[6] as u64)]);
        let b = pack("S:DXGI_SWAP_CHAIN_DESC", &v, bits, &[]);
        let flags = if bits == 64 { 64 } else { 56 };
        prop_assert_eq!(&b.bytes[flags..flags + 4], &vals[6].to_le_bytes());
        prop_assert_eq!(&b.bytes[0..4], &vals[0].to_le_bytes());
        prop_assert_eq!(b.fix, vec![(if bits == 64 { 48 } else { 44 }, replay::FIX_WINDOW, 0)]);
    }

    /// what read() gets back is what was converted, for any stream bytes it does not refuse
    #[test]
    fn streams_read_or_fail(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
        let mut b = b"IPRS".to_vec();
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&32u32.to_le_bytes());
        b.extend(bytes);
        let _ = replay::read(&b);
    }
}

/// A game killed before apitrace wrote its first chunk whole (run 42404988741: 15 trials, e.g.
/// b2013_kaguya's 77826 bytes of a 79076-byte chunk): the calls in what of that chunk is there,
/// one more the more of it there is, the same records as the whole trace's.
#[test]
fn a_trace_cut_in_its_only_chunk_keeps_its_whole_calls() {
    let ev = unpack(&d3d9_session()).unwrap();
    let mut file = b"at".to_vec();
    let z = crate::snappy::compress(&ev);
    file.extend_from_slice(&(z.len() as u32).to_le_bytes());
    file.extend(&z);
    let (whole, _) = replay::convert(&ev, Some(32)).unwrap();
    let whole = replay::read(&whole).unwrap().1;
    let mut seen = std::collections::BTreeSet::new();
    let mut last = 0;
    for n in 6..file.len() {
        let (cut_ev, cut) = unpack_cut(&file[..n]).unwrap();
        assert!(cut && ev.starts_with(&cut_ev) && cut_ev.len() >= last);
        last = cut_ev.len();
        if let Ok((out, _)) = replay::convert_cut(&cut_ev, Some(32), true) {
            let got = replay::read(&out).unwrap().1;
            assert!(got[..] == whole[..got.len()]);
            seen.insert(got.len());
        }
    }
    let calls = whole.iter().filter(|r| matches!(r, Record::Call { .. })).count();
    assert!(seen.len() >= calls, "{} record counts for {calls} calls: {seen:?}", seen.len());
}

/// A game's trace ends where the game was killed, in the middle of a chunk or of a call: what is
/// converted is the calls up to the last whole one, the same records as the whole trace's up to
/// there (at every byte it can be cut at); strictly read, a cut trace is an error.
#[test]
fn a_cut_trace_keeps_its_whole_calls() {
    let file = d3d9_session();
    let ev = unpack(&file).unwrap();
    let (whole, _) = replay::convert(&ev, Some(32)).unwrap();
    let whole = replay::read(&whole).unwrap().1;
    // (the header, properties included, has to be there: a cut inside it is an error)
    let hdr = Writer::new(&[("process.arch", "x86")]).b.len();
    let prefix_of_whole = |ev: &[u8]| {
        if ev.len() < hdr {
            assert!(replay::convert_cut(ev, Some(32), true).is_err());
            return 0;
        }
        let (out, _) = replay::convert_cut(ev, Some(32), true).unwrap();
        let got = replay::read(&out).unwrap().1;
        assert!(got.len() <= whole.len() && got[..] == whole[..got.len()], "a cut trace's records are not the whole one's");
        got.len()
    };
    let mut seen = std::collections::BTreeSet::new();
    for n in 0..=file.len() {
        let (cut_ev, cut) = unpack_cut(&file[..n.max(2)]).unwrap();
        assert_eq!(unpack(&file[..n.max(2)]).is_err(), cut);
        assert!(ev.starts_with(&cut_ev));
        seen.insert(prefix_of_whole(&cut_ev));
    }
    let mut strict_ok = 0;
    for n in 1..=ev.len() {
        let k = prefix_of_whole(&ev[..n]);
        seen.insert(k);
        // (strictly: an error, unless the bytes stop between two events)
        if let Ok((out, _)) = replay::convert(&ev[..n], Some(32)) {
            assert_eq!(replay::read(&out).unwrap().1.len(), k);
            strict_ok += 1;
        }
    }
    assert!(strict_ok < ev.len() / 4, "strict reading took {strict_ok} cuts");
    // (from nothing to all of it, one more call at a time: a call's records come with it)
    let calls = whole.iter().filter(|r| matches!(r, Record::Call { .. })).count();
    assert!(seen.contains(&0) && seen.contains(&whole.len()) && seen.len() > calls, "{seen:?}");
    assert_eq!(prefix_of_whole(&ev), whole.len());
    assert!(unpack_cut(b"xx").is_err() && unpack_cut(b"at") == Ok((vec![], false)));
}
