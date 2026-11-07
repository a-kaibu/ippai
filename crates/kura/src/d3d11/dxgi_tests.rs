//! DXGI's factory, adapters and outputs (#79): the calls two trial games' traces make on
//! Windows (testdata/apitrace/dl_RJ01720376-32, dl_RJ365650-32: the runner's two adapters,
//! a display one and WARP), every IDXGIFactory1..7 / IDXGIAdapter1..4 / IDXGIOutput1..6
//! QueryInterface, and random enumerations against a model of the adapter list.

use super::*;
use crate::d3d9::trace_tests::{alloc, seeds, Rng, SERIAL};

fn mem(n: u32) -> u32 {
    unsafe { alloc(4 * n) as u32 }
}
fn put(p: u32, v: &[u32]) {
    for (i, &x) in v.iter().enumerate() {
        unsafe { *((p + 4 * i as u32) as *mut u32) = x };
    }
}
fn rd1(p: u32) -> u32 {
    unsafe { *(p as *const u32) }
}
fn call1(id: u32, args: &[u32]) -> u32 {
    unsafe { call(id, args.as_ptr(), &crate::d3d9::trace_tests::CALLS) }
}
fn m(iface: u32, m: u32) -> u32 {
    iface << 8 | m
}
fn with<R>(f: impl FnOnce(&mut D11) -> R) -> R {
    f(D11S.lock().unwrap().as_mut().unwrap())
}
/// A GUID in guest memory (only Data1 tells interfaces apart here; the rest as Windows' IIDs)
fn iid(data1: u32) -> u32 {
    let p = mem(4);
    put(p, &[data1, 0, 0, 0]);
    p
}
fn init() {
    let vt = mem(256);
    put(vt, &(1..=256).map(|i| 0x1000 * i).collect::<Vec<_>>());
    call1(INIT, &[vt, 256]);
}
fn factory(riid: u32) -> u32 {
    let o = mem(2);
    assert_eq!(call1(CREATE_FACTORY, &[0, iid(riid), o]), 0);
    rd1(o)
}
fn refs(a: u32) -> u32 {
    with(|d| d.objs[&d.id_of(a)].refs)
}
/// GetDesc1's Description and Flags
fn desc1(ad: u32) -> (String, u32, u32, u32) {
    let p = mem(80);
    assert_eq!(call1(m(IF_ADAPTER, 10), &[ad, p]), 0);
    let name: String = (0..128)
        .map(|i| unsafe { *((p + 2 * i) as *const u16) })
        .take_while(|&c| c != 0)
        .map(|c| char::from_u32(c as u32).unwrap())
        .collect();
    let vendor = rd1(p + 256);
    let luid = rd1(p + 284);
    let flags = rd1(p + 292);
    (name, vendor, luid, flags)
}

const FACTORIES: [u32; 8] = [
    0x7b7166ec, 0x770aae78, 0x50c83a1c, 0x25483823, 0x1bc6ea02, 0x7632e1f5, 0xc1b6694f, 0xa4966eed,
];
const ADAPTERS: [u32; 5] = [0x2411e7e1, 0x29038f61, 0x0aa1ae0a, 0x645967a4, 0x3c8d99d1];
const OUTPUTS: [u32; 7] = [
    0xae02eedb, 0x00cddea8, 0x595e39d1, 0x8a6bb301, 0xdc7dca35, 0x80a07424, 0x068346e8,
];
const DXGI_OBJECT: u32 = 0xaec22fb8;

/// dl_RJ365650's start on Windows: CreateDXGIFactory2(IDXGIFactory2), QueryInterface
/// IDXGIFactory6 (S_OK, the same object), EnumAdapterByGpuPreference(0..2, HIGH_PERFORMANCE,
/// IDXGIAdapter): two adapters then NOT_FOUND, the first with output 0 only, the second (WARP,
/// "Microsoft Basic Render Driver") none; adapter 0's IDXGIAdapter1..4 (the same object);
/// D3D11CreateDevice on it. dl_RJ01720376's: the same list through EnumAdapters.
#[test]
fn trial_games_start_as_on_windows() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let f = factory(0x50c83a1c);
    let o = mem(2);
    assert_eq!(call1(m(IF_FACTORY, 0), &[f, iid(0xc1b6694f), o]), 0);
    assert_eq!(rd1(o), f);
    call1(m(IF_FACTORY, 2), &[f]);
    let mut got = Vec::new();
    for i in 0..3 {
        let o = mem(2);
        let r = call1(m(IF_FACTORY, 29), &[f, i, 2, iid(0x2411e7e1), o]);
        if r != 0 {
            assert_eq!((r, rd1(o)), (DXGI_ERROR_NOT_FOUND, 0));
            got.push((r, String::new(), 0));
            continue;
        }
        let ad = rd1(o);
        let (name, ..) = desc1(ad);
        let mut outputs = 0;
        loop {
            let oo = mem(2);
            let r = call1(m(IF_ADAPTER, 7), &[ad, outputs, oo]);
            if r != 0 {
                assert_eq!((r, rd1(oo)), (DXGI_ERROR_NOT_FOUND, 0));
                break;
            }
            outputs += 1;
            call1(m(IF_OUTPUT, 2), &[rd1(oo)]);
        }
        got.push((r, name, outputs));
        assert_eq!(call1(m(IF_ADAPTER, 2), &[ad]), 0);
    }
    assert_eq!(got[0].0, 0);
    assert_eq!(got[0].2, 1);
    assert_eq!(got[1], (0, "Microsoft Basic Render Driver".to_string(), 0));
    assert_eq!(got[2].0, DXGI_ERROR_NOT_FOUND);
    // (the same list through EnumAdapters and EnumAdapters1)
    for e in [7, 12] {
        for i in 0..3 {
            let o = mem(2);
            let r = call1(m(IF_FACTORY, e), &[f, i, o]);
            assert_eq!(
                r,
                if i < 2 { 0 } else { DXGI_ERROR_NOT_FOUND },
                "EnumAdapters{} {i}",
                if e == 12 { "1" } else { "" }
            );
            if r == 0 {
                assert_eq!(desc1(rd1(o)).0 == "Microsoft Basic Render Driver", i == 1);
                call1(m(IF_ADAPTER, 2), &[rd1(o)]);
            }
        }
    }
    // adapter 0 as IDXGIAdapter1..4, and a device made on it
    let o = mem(2);
    assert_eq!(call1(m(IF_FACTORY, 29), &[f, 0, 2, iid(0x2411e7e1), o]), 0);
    let ad = rd1(o);
    for x in ADAPTERS {
        let q = mem(2);
        assert_eq!(call1(m(IF_ADAPTER, 0), &[ad, iid(x), q]), 0, "{x:08x}");
        assert_eq!(rd1(q), ad);
        call1(m(IF_ADAPTER, 2), &[ad]);
    }
    let (dev_out, level, ctx_out) = (mem(2), mem(2), mem(2));
    assert_eq!(
        call1(
            CREATE_DEVICE,
            &[ad, 0, 0, 0, 0, 0, 7, 0, 0, dev_out, level, ctx_out]
        ),
        0
    );
    call1(m(IF_CONTEXT, 2), &[rd1(ctx_out)]);
    call1(m(IF_DEVICE, 2), &[rd1(dev_out)]);
    assert_eq!(call1(m(IF_ADAPTER, 2), &[ad]), 0);
    assert_eq!(call1(m(IF_FACTORY, 2), &[f]), 0);
}

/// Every factory, adapter and output interface DXGI 1.6 has answers on its object (and on no
/// other), the descriptions' layouts, and the Factory4..7 / Output1..6 calls.
#[test]
fn every_dxgi_interface() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    init();
    for x in FACTORIES {
        let f = factory(x);
        for y in FACTORIES.iter().chain(&[DXGI_OBJECT, 0]) {
            let q = mem(2);
            assert_eq!(
                call1(m(IF_FACTORY, 0), &[f, iid(*y), q]),
                0,
                "factory {x:08x} as {y:08x}"
            );
            call1(m(IF_FACTORY, 2), &[rd1(q)]);
        }
        for y in ADAPTERS.iter().chain(&OUTPUTS) {
            let q = mem(2);
            assert_eq!(call1(m(IF_FACTORY, 0), &[f, iid(*y), q]), E_NOINTERFACE);
            assert_eq!(rd1(q), 0);
        }
        // (EnumWarpAdapter is WARP, as EnumAdapters' last)
        let o = mem(2);
        assert_eq!(call1(m(IF_FACTORY, 27), &[f, iid(0x3c8d99d1), o]), 0);
        let w = rd1(o);
        let (name, vendor, luid, flags) = desc1(w);
        assert_eq!(
            (name.as_str(), vendor, flags),
            ("Microsoft Basic Render Driver", 0x1414, 2)
        );
        // EnumAdapterByLuid of each adapter's LUID: a new object of that adapter (as on Windows,
        // every enumeration makes one)
        let q = mem(2);
        assert_eq!(
            call1(m(IF_FACTORY, 26), &[f, luid, 0, iid(0x2411e7e1), q]),
            0
        );
        assert_ne!(rd1(q), w);
        assert_eq!(desc1(rd1(q)), desc1(w));
        call1(m(IF_ADAPTER, 2), &[rd1(q)]);
        assert_eq!(
            call1(m(IF_FACTORY, 26), &[f, 0x777, 0, iid(0x2411e7e1), q]),
            DXGI_ERROR_NOT_FOUND
        );
        assert_eq!(rd1(q), 0);
        assert_eq!(call1(m(IF_ADAPTER, 7), &[w, 0, q]), DXGI_ERROR_NOT_FOUND);
        call1(m(IF_ADAPTER, 2), &[w]);
        // CheckFeatureSupport(ALLOW_TEARING): FALSE; a wrong size, E_INVALIDARG
        let b = mem(1);
        put(b, &[7]);
        assert_eq!(call1(m(IF_FACTORY, 28), &[f, 0, b, 4]), 0);
        assert_eq!(rd1(b), 0);
        assert_eq!(
            call1(m(IF_FACTORY, 28), &[f, 0, b, 8]),
            DXGI_ERROR_INVALID_CALL
        );
        // (a preference past HIGH_PERFORMANCE: enumerated as UNSPECIFIED)
        assert_eq!(call1(m(IF_FACTORY, 29), &[f, 0, 3, iid(0x2411e7e1), q]), 0);
        call1(m(IF_ADAPTER, 2), &[rd1(q)]);
        assert_eq!(call1(m(IF_FACTORY, 2), &[f]), 0);
    }
    let f = factory(0xa4966eed);
    let o = mem(2);
    assert_eq!(call1(m(IF_FACTORY, 12), &[f, 0, o]), 0);
    let ad = rd1(o);
    // DESC2 / DESC3: DESC1's fields, then the preemption granularities
    for meth in [11, 18] {
        let p = mem(80);
        assert_eq!(call1(m(IF_ADAPTER, meth), &[ad, p]), 0);
        assert_eq!(rd1(p + 256), 0x10de);
        assert_eq!((rd1(p + 292), rd1(p + 296), rd1(p + 300)), (0, 1, 1));
    }
    let info = mem(8);
    assert_eq!(call1(m(IF_ADAPTER, 15), &[ad, 0, 0, info]), 0);
    assert!(rd1(info) != 0, "local budget");
    assert_eq!(call1(m(IF_ADAPTER, 15), &[ad, 0, 2, info]), E_INVALIDARG);
    assert_eq!(call1(m(IF_ADAPTER, 7), &[ad, 0, o]), 0);
    let out = rd1(o);
    for y in OUTPUTS.iter().chain(&[DXGI_OBJECT]) {
        let q = mem(2);
        assert_eq!(
            call1(m(IF_OUTPUT, 0), &[out, iid(*y), q]),
            0,
            "output as {y:08x}"
        );
        assert_eq!(rd1(q), out);
        call1(m(IF_OUTPUT, 2), &[out]);
    }
    // GetDesc1: GetDesc's, then 8 bits per color and the sRGB primaries
    let p = mem(40);
    assert_eq!(call1(m(IF_OUTPUT, 27), &[out, p]), 0);
    assert_eq!(unsafe { *(p as *const u16) }, b'\\' as u16);
    assert_eq!((rd1(p + 84), rd1(p + 92), rd1(p + 96)), (1, 8, 0));
    assert_eq!(f32::from_bits(rd1(p + 100)), 0.64);
    // GetDisplayModeList1: the modes GetDisplayModeList has, 32 bytes each
    let (n0, n1) = (mem(1), mem(1));
    assert_eq!(call1(m(IF_OUTPUT, 8), &[out, 28, 0, n0, 0]), 0);
    assert_eq!(call1(m(IF_OUTPUT, 19), &[out, 28, 0, n1, 0]), 0);
    assert_eq!(rd1(n0), rd1(n1));
    let modes = mem(8 * rd1(n1));
    assert_eq!(call1(m(IF_OUTPUT, 19), &[out, 28, 0, n1, modes]), 0);
    let last = modes + 32 * (rd1(n1) - 1);
    assert_eq!((rd1(last), rd1(last + 16), rd1(last + 28)), (1920, 28, 0));
    assert_eq!(call1(m(IF_OUTPUT, 23), &[out]), 0);
    call1(m(IF_OUTPUT, 2), &[out]);
    call1(m(IF_ADAPTER, 2), &[ad]);
    call1(m(IF_FACTORY, 2), &[f]);
}

/// Random EnumAdapters / EnumAdapters1 / EnumAdapterByGpuPreference / EnumAdapterByLuid /
/// EnumWarpAdapter calls against the list [GPU, WARP]: the index past it NOT_FOUND with a null
/// out, every call a new object (as on Windows) of the right adapter, its LUID found again, and
/// each object freed when the application lets go of it.
#[test]
fn adapter_enumeration_model() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    init();
    for seed in 0..seeds() * 10 {
        let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let f = factory(FACTORIES[r.below(FACTORIES.len())]);
        let mut held: Vec<(u32, usize)> = Vec::new();
        let mut seen: [u32; 2] = [0; 2];
        for _ in 0..40 {
            let o = mem(2);
            put(o, &[0xdead_beef]);
            let i = r.below(4) as u32;
            let (res, want) = match r.below(6) {
                0 => (call1(m(IF_FACTORY, 7), &[f, i, o]), i),
                1 => (call1(m(IF_FACTORY, 12), &[f, i, o]), i),
                2 => (
                    call1(
                        m(IF_FACTORY, 29),
                        &[f, i, r.below(3) as u32, iid(ADAPTERS[r.below(5)]), o],
                    ),
                    i,
                ),
                3 => (
                    call1(m(IF_FACTORY, 27), &[f, iid(ADAPTERS[r.below(5)]), o]),
                    1,
                ),
                4 if !held.is_empty() => {
                    let (a, _) = held[r.below(held.len())];
                    let (_, _, luid, _) = desc1(a);
                    let k = r.below(2) as usize;
                    // (a LUID of no adapter: NOT_FOUND)
                    let l = if k == 0 { luid } else { luid ^ 0x40 };
                    let res = call1(m(IF_FACTORY, 26), &[f, l, 0, iid(ADAPTERS[r.below(5)]), o]);
                    if k == 1 {
                        assert_eq!((res, rd1(o)), (DXGI_ERROR_NOT_FOUND, 0));
                        continue;
                    }
                    (res, held.iter().find(|h| h.0 == a).unwrap().1 as u32)
                }
                _ => {
                    if let Some(k) = (!held.is_empty()).then(|| r.below(held.len())) {
                        let (a, _) = held.swap_remove(k);
                        assert_eq!(call1(m(IF_ADAPTER, 2), &[a]), 0, "seed {seed}: freed");
                    }
                    continue;
                }
            };
            if want >= 2 {
                assert_eq!((res, rd1(o)), (DXGI_ERROR_NOT_FOUND, 0), "seed {seed}");
                continue;
            }
            assert_eq!(res, 0, "seed {seed}");
            let a = rd1(o);
            assert!(held.iter().all(|h| h.0 != a), "seed {seed}: a new object");
            let luid = desc1(a).2;
            if seen[want as usize] == 0 {
                seen[want as usize] = luid;
            }
            assert_eq!(
                luid, seen[want as usize],
                "seed {seed}: adapter {want}'s LUID"
            );
            assert_eq!(desc1(a).3, if want == 1 { 2 } else { 0 });
            assert_eq!(refs(a), 1, "seed {seed}");
            held.push((a, want as usize));
        }
        for (a, _) in held {
            assert_eq!(call1(m(IF_ADAPTER, 2), &[a]), 0, "seed {seed}");
        }
        assert_eq!(call1(m(IF_FACTORY, 2), &[f]), 0, "seed {seed}");
    }
}
