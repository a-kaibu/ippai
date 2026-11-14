//! A program's several Direct3D 9 devices (KiriKiri Z makes one per window and releases a small
//! window's while the main one draws on: 姫騎士ド下品敗北人生's title was black, every call on the
//! main device D3DERR_INVALIDCALL once the other was gone). Random runs of CreateDevice, Release,
//! SetRenderState / GetRenderState, BeginScene / EndScene, CreateTexture and the texture's
//! GetDevice on up to four devices, through the real entry point, against a model: each live
//! device keeps its own state and scene, a live device's calls never fail for another's sake,
//! and an object answers with the device that made it. (tests/winref/probes/d3d9twodev.c draws
//! with two of them on Windows and on sake.)

use super::trace_tests::{alloc, CALLS, SERIAL};
use super::*;
use proptest::prelude::*;

fn call(iface: u32, m: u32, args: &[u32]) -> u32 {
    let mut a = args.to_vec();
    a.resize(a.len().max(16), 0);
    unsafe { vnhost_d3d9_call(iface << 8 | m, a.as_ptr(), &CALLS) }
}
fn mem(n: u32) -> u32 {
    unsafe { alloc(4 * n) as u32 }
}
fn rd(p: u32) -> u32 {
    unsafe { *(p as *const u32) }
}
fn wr(p: u32, i: u32, v: u32) {
    unsafe { *((p + 4 * i) as *mut u32) = v }
}

#[derive(Clone, Debug)]
enum Op {
    Create,
    Release(usize),
    Set(usize, u32),
    Get(usize),
    Begin(usize),
    End(usize),
    Texture(usize),
    TexDevice(usize),
    /// SetFVF and GetVertexDeclaration (the device makes a declaration of the FVF), released
    Fvf(usize, u32),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        1 => Just(Op::Create),
        1 => (0usize..4).prop_map(Op::Release),
        3 => (0usize..4, 0u32..256).prop_map(|(i, v)| Op::Set(i, v)),
        3 => (0usize..4).prop_map(Op::Get),
        2 => (0usize..4).prop_map(Op::Begin),
        2 => (0usize..4).prop_map(Op::End),
        1 => (0usize..4).prop_map(Op::Texture),
        2 => (0usize..4).prop_map(Op::TexDevice),
        2 => (0usize..4, prop::sample::select(vec![0x002u32, 0x042, 0x142, 0x044, 0x152, 0x102])).prop_map(|(i, f)| Op::Fvf(i, f)),
    ]
}

/// a live device in the model: its object, ALPHAREF, in a scene, its textures
struct Live {
    dev: u32,
    alpharef: u32,
    scene: bool,
    textures: Vec<u32>,
}

fn objects() -> usize {
    D3D.lock().unwrap().as_ref().map_or(0, |d| d.objs.len())
}

fn run(ops: &[Op]) -> Result<(), TestCaseError> {
    let at_start = objects();
    let o = mem(4);
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o);
    let pp = mem(14);
    // (16x16 X8R8G8B8, one back buffer, DISCARD, windowed)
    for (i, x) in [16, 16, 22, 1, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0].into_iter().enumerate() {
        wr(pp, i as u32, x);
    }
    let mut live: Vec<Live> = Vec::new();
    for op in ops {
        let pick = |i: usize| if live.is_empty() { None } else { Some(i % live.len()) };
        match *op {
            Op::Create => {
                if live.len() < 4 {
                    prop_assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x20, pp, o]), 0);
                    // (ALPHAREF starts at 0)
                    live.push(Live { dev: rd(o), alpharef: 0, scene: false, textures: Vec::new() });
                }
            }
            Op::Release(i) => {
                if let Some(i) = pick(i) {
                    let l = live.remove(i);
                    for t in l.textures {
                        prop_assert_eq!(call(IF_TEXTURE, 2, &[t]), 0);
                    }
                    prop_assert_eq!(call(IF_DEVICE, 2, &[l.dev]), 0);
                }
            }
            Op::Set(i, v) => {
                if let Some(i) = pick(i) {
                    prop_assert_eq!(call(IF_DEVICE, 57, &[live[i].dev, 24, v]), 0);
                    live[i].alpharef = v;
                }
            }
            Op::Get(i) => {
                if let Some(i) = pick(i) {
                    wr(o, 0, 0xdead);
                    prop_assert_eq!(call(IF_DEVICE, 58, &[live[i].dev, 24, o]), 0);
                    prop_assert_eq!(rd(o), live[i].alpharef, "device {} of {}", i, live.len());
                }
            }
            Op::Begin(i) => {
                if let Some(i) = pick(i) {
                    let want = if live[i].scene { D3DERR_INVALIDCALL } else { 0 };
                    prop_assert_eq!(call(IF_DEVICE, 41, &[live[i].dev]), want);
                    live[i].scene = true;
                }
            }
            Op::End(i) => {
                if let Some(i) = pick(i) {
                    let want = if live[i].scene { 0 } else { D3DERR_INVALIDCALL };
                    prop_assert_eq!(call(IF_DEVICE, 42, &[live[i].dev]), want);
                    live[i].scene = false;
                }
            }
            Op::Texture(i) => {
                if let Some(i) = pick(i) {
                    // (8x8, 1 level, A8R8G8B8, MANAGED)
                    prop_assert_eq!(call(IF_DEVICE, 23, &[live[i].dev, 8, 8, 1, 0, 21, 1, o, 0]), 0);
                    live[i].textures.push(rd(o));
                }
            }
            Op::Fvf(i, fvf) => {
                if let Some(i) = pick(i) {
                    prop_assert_eq!(call(IF_DEVICE, 89, &[live[i].dev, fvf]), 0);
                    wr(o, 0, 0);
                    prop_assert_eq!(call(IF_DEVICE, 88, &[live[i].dev, o]), 0);
                    prop_assert!(rd(o) != 0);
                    call(IF_DECL, 2, &[rd(o)]);
                }
            }
            Op::TexDevice(i) => {
                if let Some(i) = pick(i) {
                    if let Some(&t) = live[i].textures.last() {
                        // (GetDevice: the device that made it, with a reference)
                        prop_assert_eq!(call(IF_TEXTURE, 3, &[t, o]), 0);
                        prop_assert_eq!(rd(o), live[i].dev);
                        call(IF_DEVICE, 2, &[live[i].dev]);
                        // (and its LockRect works whatever the other devices did)
                        let lr = mem(2);
                        prop_assert_eq!(call(IF_TEXTURE, 19, &[t, 0, lr, 0, 0]), 0);
                        prop_assert_eq!(call(IF_TEXTURE, 20, &[t, 0]), 0);
                    }
                }
            }
        }
    }
    for l in live {
        for t in l.textures {
            call(IF_TEXTURE, 2, &[t]);
        }
        prop_assert_eq!(call(IF_DEVICE, 2, &[l.dev]), 0);
    }
    prop_assert_eq!(call(IF_D3D9, 2, &[d3d]), 0);
    // (everything the devices made for themselves went with them)
    prop_assert_eq!(objects(), at_start, "objects left after every device was released");
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]
    #[test]
    fn devices_are_separate(ops in proptest::collection::vec(op(), 1..60)) {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
            vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
        }
        run(&ops)?;
    }
}

/// The game's order: the main device draws, a second one is made and released, the main one
/// keeps drawing (each call of it S_OK, its state its own)
#[test]
fn second_device_released_while_the_first_draws() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    use Op::*;
    let r = run(&[Create, Set(0, 7), Begin(0), End(0), Create, Set(1, 9), Begin(1), End(1), Release(1), Get(0), Begin(0), End(0), Begin(0), End(0), Get(0)]);
    assert!(r.is_ok(), "{r:?}");
}
