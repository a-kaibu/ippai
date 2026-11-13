//! Direct3D 8's device caps (d3d8.rs caps8_of): D3DCAPS8 is D3DCAPS9's first 53 dwords with
//! Direct3D 8's shader versions and D3DCAPS2_CANRENDERWINDOWED, which d3d9 dropped and
//! Windows' d3d8 sets. The DirectX 8 SDK's D3DApp framework (CD3DApplication::BuildDeviceList,
//! 凌●学園 体験版's gakuentr.exe) keeps a device only when its caps say it renders windowed;
//! without it no device is left and the game says the video driver lacks DirectX 8.
//!
//! - property: every other dword of any D3DCAPS9 reaches D3DCAPS8 unchanged;
//! - the interfaces: IDirect3D8::GetDeviceCaps (HAL, REF) and IDirect3DDevice8::GetDeviceCaps;
//! - the framework's enumeration, as the game runs it, finds a windowed HAL device;
//! - Windows' answer (tests/winref/golden/d3d8-32 "caps2" lines) once the winref workflow has it.

use super::d3d8::{caps8_of, CAPS2_CANRENDERWINDOWED, CREATE};
use super::draw_tests::{call, out, rd};
use super::trace_tests::{alloc, attach_page, on_low_stack, CALLS, SERIAL};
use super::*;
use proptest::prelude::*;

const V8: u32 = 16;

proptest! {
    #[test]
    fn caps8_keeps_caps9_but_shaders_and_windowed(c in proptest::collection::vec(any::<u32>(), 76)) {
        let c: [u32; 76] = c.try_into().unwrap();
        let o = caps8_of(&c);
        for i in 0..53 {
            match i {
                3 => prop_assert_eq!(o[3], c[3] | CAPS2_CANRENDERWINDOWED),
                49 => prop_assert_eq!(o[49], 0xfffe_0101),
                50 => prop_assert_eq!(o[50], 96),
                51 => prop_assert_eq!(o[51], 0xffff_0104),
                _ => prop_assert_eq!(o[i], c[i], "dword {}", i),
            }
        }
    }
}

fn init() {
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
}

fn d3d8() -> u32 {
    attach_page();
    let o = out();
    assert_eq!(unsafe { vnhost_d3d9_call(CREATE, [o].as_ptr(), &CALLS) }, 0);
    rd(o)
}

/// D3DCAPS8 (53 dwords) of IDirect3D8::GetDeviceCaps(0, ty)
fn caps(d3d: u32, ty: u32) -> (u32, Vec<u32>) {
    let p = unsafe { alloc(53 * 4) as u32 };
    let r = call(IF_D3D9 + V8, 13, &[d3d, 0, ty, p]);
    (r, (0..53).map(|i| unsafe { *((p + 4 * i) as *const u32) }).collect())
}

#[test]
fn d3d8_devices_render_windowed() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    on_low_stack(|| {
        init();
        let d3d = d3d8();
        for ty in [1, 2] {
            let (r, c) = caps(d3d, ty);
            assert_eq!(r, 0);
            assert_eq!(c[0], ty, "DeviceType");
            assert_ne!(c[3] & CAPS2_CANRENDERWINDOWED, 0, "type {ty}: Caps2 {:#x}", c[3]);
            assert_eq!((c[49], c[51]), (0xfffe_0101, 0xffff_0104));
        }
        // the device's own GetDeviceCaps
        let pp = unsafe { alloc(52) as u32 };
        for (i, x) in [64, 64, 22, 1, 0, 1, 1, 1, 0, 0, 0, 0, 0].into_iter().enumerate() {
            unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
        }
        let o = out();
        assert_eq!(call(IF_D3D9 + V8, 15, &[d3d, 0, 1, 1, 0x20, pp, o]), 0);
        let dev = rd(o);
        let p = unsafe { alloc(53 * 4) as u32 };
        assert_eq!(call(IF_DEVICE + V8, 7, &[dev, p]), 0);
        assert_ne!(unsafe { *((p + 12) as *const u32) } & CAPS2_CANRENDERWINDOWED, 0);
        call(IF_DEVICE + V8, 2, &[dev]);
        gpu::set_pump(None);
    });
}

/// CD3DApplication::BuildDeviceList of the DirectX 8 SDK as gakuentr.exe runs it (0x403a10):
/// the adapter's display-mode formats of the modes at least 640x480 (and the desktop's), per
/// device type HAL then REF: GetDeviceCaps, CheckDeviceType(fmt, fmt, fullscreen) per format,
/// a vertex processing ConfirmDevice accepts; a device counts when it has modes, and it is
/// usable windowed only when Caps2 has CANRENDERWINDOWED. The game then takes the first device
/// that is (0x4038c0: else 0x82000004 and the "video driver" message); a REF one is refused too.
#[test]
fn d3dapp_enumeration_finds_a_windowed_hal_device() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    on_low_stack(|| {
        init();
        let d3d = d3d8();
        let mode = unsafe { alloc(16) as u32 };
        let rdm = |i: u32| unsafe { *((mode + 4 * i) as *const u32) };
        assert_eq!(call(IF_D3D9 + V8, 8, &[d3d, 0, mode]), 0);
        let mut formats = vec![rdm(3)];
        let mut modes = Vec::new();
        let n = call(IF_D3D9 + V8, 6, &[d3d, 0]);
        assert!(n > 0);
        for i in 0..n {
            assert_eq!(call(IF_D3D9 + V8, 7, &[d3d, 0, i, mode]), 0);
            if rdm(0) >= 640 && rdm(1) >= 480 {
                modes.push((rdm(0), rdm(1), rdm(3)));
                if !formats.contains(&rdm(3)) {
                    formats.push(rdm(3));
                }
            }
        }
        let mut chosen = None;
        for ty in [1u32, 2] {
            let (r, c) = caps(d3d, ty);
            assert_eq!(r, 0);
            let ok: Vec<u32> = formats.iter().copied().filter(|&f| call(IF_D3D9 + V8, 9, &[d3d, 0, ty, f, f, 0]) == 0).collect();
            let n_modes = modes.iter().filter(|m| ok.contains(&m.2)).count();
            let windowed = ok.contains(&formats[0]) && c[3] & CAPS2_CANRENDERWINDOWED != 0;
            if n_modes > 0 && windowed && chosen.is_none() {
                chosen = Some(ty);
            }
        }
        assert_eq!(chosen, Some(1), "no windowed HAL device (formats {formats:?}, {} modes)", modes.len());
    });
}

/// Windows' Caps2 bits (the d3d8 probe's "caps2" lines), when the winref workflow recorded them.
#[test]
fn d3d8_caps2_is_windows() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    on_low_stack(|| {
        let p = format!("{}/../../tests/winref/golden/d3d8-32/stdout.txt", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}"));
        init();
        let d3d = d3d8();
        for l in text.lines().filter(|l| l.starts_with("caps2 ")) {
            // "caps2 HAL windowed 1 gamma 1 dynamic 1"
            let w: Vec<&str> = l.split_whitespace().collect();
            let ty = if w[1] == "HAL" { 1 } else { 2 };
            let (r, c) = caps(d3d, ty);
            let mine = if r != 0 {
                format!("caps2 {} {r:#x}", w[1])
            } else {
                format!(
                    "caps2 {} windowed {} gamma {} dynamic {}",
                    w[1],
                    (c[3] & CAPS2_CANRENDERWINDOWED != 0) as u32,
                    (c[3] & 0x2_0000 != 0) as u32,
                    (c[3] & 0x2000_0000 != 0) as u32
                )
            };
            assert_eq!(mine, l);
        }
    });
}
