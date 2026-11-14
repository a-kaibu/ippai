//! dinput8.dll / dinput.dll: the system keyboard and mouse as DirectInput devices, read from
//! user32's key state and cursor, plus buffered data (GetDeviceData) queued from user32's input
//! events. Joysticks are not there (EnumDevices lists none).

#![allow(non_snake_case)]

use crate::HashMap;
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use crate::api::{implement, Ctx, Handler};
use crate::mem::{self, r32, w32};

const KEYBOARD: u32 = 0x6F1D_2B61;
const MOUSE: u32 = 0x6F1D_2B60;

use crate::api::vtable;

struct Dev {
    kind: u32,
    /// the cursor at the last GetDeviceState (relative motion)
    last: (i32, i32),
    /// wheel motion since the last GetDeviceState
    wheel: i32,
    /// DIPROP_BUFFERSIZE; 0: not buffered
    cap: u32,
    /// buffered data: (offset, data, time, sequence)
    buf: VecDeque<[u32; 4]>,
    /// the cursor at the last buffered motion
    at: (i32, i32),
    /// the interface's reference count: the device goes (with its queue and its block) at 0
    refs: u32,
    /// made by a wide (W) DirectInput object, an ANSI one, or one whose kind is not known
    /// (CoCreateInstance): which DIDEVICEINSTANCE sizes GetDeviceInfo takes
    wide: Option<bool>,
}

/// device object -> its state
static DEVICES: Mutex<Option<HashMap<u32, Dev>>> = Mutex::new(None);
static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

fn devices<R>(f: impl FnOnce(&mut HashMap<u32, Dev>) -> R) -> R {
    f(DEVICES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::default))
}

/// user32's input event (its kinds: 1 move, 2/3 button down/up, 4 wheel, 5/6 key down/up),
/// queued on the devices that buffer and counted for the mouse state's wheel.
pub fn input(kind: u32, x: i32, y: i32, data: u32) {
    let time = crate::k32::ticks_ms();
    devices(|ds| {
        for d in ds.values_mut() {
            let mut ev: [(u32, u32); 2] = [(u32::MAX, 0); 2];
            match (d.kind, kind) {
                (MOUSE, 1) => {
                    let (dx, dy) = (x - d.at.0, y - d.at.1);
                    d.at = (x, y);
                    if dx != 0 {
                        ev[0] = (0, dx as u32);
                    }
                    if dy != 0 {
                        ev[1] = (4, dy as u32);
                    }
                }
                (MOUSE, 2 | 3) if data < 3 => ev[0] = (12 + data, if kind == 2 { 0x80 } else { 0 }),
                (MOUSE, 4) => {
                    d.wheel += data as i32;
                    ev[0] = (8, data);
                }
                (KEYBOARD, 5 | 6) => {
                    if let Some(k) = dik_of(data as usize & 0xff) {
                        ev[0] = (k as u32, if kind == 5 { 0x80 } else { 0 });
                    }
                }
                _ => {}
            }
            for (ofs, v) in ev {
                if ofs == u32::MAX || d.cap == 0 {
                    continue;
                }
                if d.buf.len() >= d.cap as usize {
                    d.buf.pop_front();
                }
                d.buf.push_back([ofs, v, time, SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)]);
            }
        }
    });
}

fn ok(_: &mut Ctx) -> u32 {
    0
}
fn add_ref(_: &mut Ctx) -> u32 {
    1
}
fn release(_: &mut Ctx) -> u32 {
    0
}
/// A device's AddRef / Release: at the last Release the device leaves the table (no more input
/// queued on it) and its block is freed, as Windows frees it (games that create the keyboard
/// again on every focus change or scene kept every old one fed with every event).
fn dev_add_ref(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    devices(|ds| ds.get_mut(&o).map_or(0, |d| {
        d.refs = d.refs.saturating_add(1);
        d.refs
    }))
}
fn dev_release(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    let left = devices(|ds| {
        let d = ds.get_mut(&o)?;
        d.refs -= 1;
        let left = d.refs;
        if left == 0 {
            ds.remove(&o);
        }
        Some(left)
    });
    if left == Some(0) {
        mem::heap_free(o);
    }
    left.unwrap_or(0)
}
fn no_interface(c: &mut Ctx) -> u32 {
    mem::putp(c.a(2), 0);
    0x8000_4002
}

/// IDirectInput8, or (`v7`) IDirectInput7, whose slot 9 is CreateDeviceEx.
fn di_vtbl(v7: bool) -> u32 {
    static V8: OnceLock<u32> = OnceLock::new();
    static V7: OnceLock<u32> = OnceLock::new();
    if v7 {
        return *V7.get_or_init(|| {
            vtable(
                "IDirectInput7",
                &[
                    (no_interface, 12),
                    (add_ref, 4),
                    (release, 4),
                    (CreateDevice, 16),
                    (EnumDevices, 20),
                    (GetDeviceStatus, 8),
                    (ok, 8),
                    (ok, 12),
                    (FindDevice, 16),
                    (CreateDeviceEx, 20),
                ],
            )
        });
    }
    *V8.get_or_init(|| {
        vtable(
            "IDirectInput8",
            &[
                (no_interface, 12),
                (add_ref, 4),
                (release, 4),
                (CreateDevice, 16),
                (EnumDevices, 20),
                (GetDeviceStatus, 8),
                (ok, 8),
                (ok, 12),
                (FindDevice, 16),
                (ok, 24),
                (ok, 20),
            ],
        )
    })
}

fn dev_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        let mut m: Vec<(Handler, u32)> = vec![
            (no_interface, 12),
            (dev_add_ref, 4),
            (dev_release, 4),
            (GetCapabilities, 8),
            (ok, 20),
            (GetProperty, 12),
            (SetProperty, 12),
            (Acquire, 4),
            (ok, 4),
            (GetDeviceState, 12),
            (GetDeviceData, 20),
            (ok, 8),
            (ok, 8),
            (ok, 12),
            (unsupported, 16),
            (GetDeviceInfo, 8),
            (ok, 12),
            (ok, 16),
            (unsupported, 20),
            (ok, 16),
            (unsupported, 12),
            (unsupported, 8),
            (unsupported, 8),
            (ok, 16),
            (unsupported, 8),
            (ok, 4),
            (unsupported, 20),
            (unsupported, 20),
            (unsupported, 20),
            (unsupported, 16),
            (unsupported, 16),
            (unsupported, 8),
        ];
        m.truncate(32);
        vtable("IDirectInputDevice8", &m)
    })
}

fn unsupported(_: &mut Ctx) -> u32 {
    0x8000_4001
}

/// A DirectInput object: its vtable, then (at 4 in the block, not a field the program sees)
/// 1 ANSI, 2 wide, 0 not known.
fn create_di(out: u32, v7: bool, wide: Option<bool>) -> u32 {
    let o = mem::heap_alloc(12, true);
    mem::wp(o, di_vtbl(v7));
    w32(o + mem::ps(), wide.map_or(0, |w| 1 + w as u32));
    mem::putp(out, o);
    0
}
fn di_wide(o: u32) -> Option<bool> {
    match if mem::heap_owns(o) { r32(o + mem::ps()) } else { 0 } {
        1 => Some(false),
        2 => Some(true),
        _ => None,
    }
}
/// Whether an IDirectInput(2/7/8) IID is of the W interface (None: not one of them)
fn iid_wide(iid: u32) -> Option<bool> {
    match if iid == 0 { 0 } else { r32(iid) } {
        0xbf79_8030 | 0x9a4c_b684 | 0x5944_e662 | 0x8952_1360 => Some(false),
        0xbf79_8031 | 0x9a4c_b685 | 0x5944_e663 | 0x8952_1361 => Some(true),
        _ => None,
    }
}

fn DirectInput8Create(c: &mut Ctx) -> u32 {
    create_di(c.a(3), false, iid_wide(c.a(2)))
}
fn DirectInputCreateA(c: &mut Ctx) -> u32 {
    create_di(c.a(2), true, Some(false))
}
fn DirectInputCreateW(c: &mut Ctx) -> u32 {
    create_di(c.a(2), true, Some(true))
}
fn DirectInputCreateEx(c: &mut Ctx) -> u32 {
    create_di(c.a(3), true, iid_wide(c.a(2)))
}

/// CoCreateInstance of CLSID_DirectInput / CLSID_DirectInput8 (then Initialize).
pub fn create_instance(clsid: u32) -> Option<u32> {
    let v7 = match mem::r32(clsid) {
        0x25e6_09e0 => true,
        0x25e6_09e4 => false,
        _ => return None,
    };
    let out = mem::heap_alloc(8, true);
    create_di(out, v7, None);
    let o = mem::r32(out);
    mem::heap_free(out);
    Some(o)
}

fn CreateDeviceEx(c: &mut Ctx) -> u32 {
    let (guid, out) = (c.a(1), c.a(3));
    create_device(guid, out, di_wide(c.a(0)))
}

fn CreateDevice(c: &mut Ctx) -> u32 {
    create_device(c.a(1), c.a(2), di_wide(c.a(0)))
}
fn create_device(guid: u32, out: u32, wide: Option<bool>) -> u32 {
    let kind = r32(guid);
    if kind != KEYBOARD && kind != MOUSE {
        mem::putp(out, 0);
        return 0x8007_0002;
    }
    let o = mem::heap_alloc(8, true);
    mem::wp(o, dev_vtbl());
    let at = crate::user::cursor_pos();
    devices(|ds| ds.insert(o, Dev { kind, last: at, wheel: 0, cap: 0, buf: VecDeque::new(), at, refs: 1, wide }));
    mem::putp(out, o);
    0
}

fn write_guid(p: u32, first: u32) {
    w32(p, first);
    mem::write(p + 4, &[0xa0, 0xd5, 0xcf, 0x11, 0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0, 0]);
}

/// The DIDEVICEINSTANCE layouts by dwSize: DirectX 3's (0x230 ANSI, 0x438 wide: no guidFFDriver,
/// wUsagePage, wUsage) and DirectX 5's (0x244, 0x44c); `Some(wide)`, None for any other size or
/// one of the other character set than the device's (`wide`, when it is known).
fn instance_layout(size: u32, wide: Option<bool>) -> Option<bool> {
    let w = match size {
        0x230 | 0x244 => false,
        0x438 | 0x44c => true,
        _ => return None,
    };
    wide.map_or(true, |d| d == w).then_some(w)
}

/// DIDEVICEINSTANCE(A/W) of `size` bytes (one of instance_layout's): nothing is written past
/// it (a DirectX 3 program's 0x230-byte structure on its stack used to get 0x244 bytes, over
/// its saved registers).
fn device_instance(p: u32, kind: u32, size: u32) {
    let Some(wide) = instance_layout(size, None) else { return };
    mem::fill(p, size, 0);
    w32(p, size);
    write_guid(p + 4, kind);
    write_guid(p + 20, kind);
    w32(p + 36, if kind == KEYBOARD { 0x413 } else { 0x212 });
    let name: &[u8] = if kind == KEYBOARD { b"Keyboard" } else { b"Mouse" };
    if wide {
        for (i, &b) in name.iter().enumerate() {
            mem::w16(p + 40 + 2 * i as u32, b as u16);
            mem::w16(p + 560 + 2 * i as u32, b as u16);
        }
    } else {
        mem::write(p + 40, name);
        mem::write(p + 300, name);
    }
}

fn EnumDevices(c: &mut Ctx) -> u32 {
    let (ty, cb, refd) = (c.a(1), c.a(2), c.a(3));
    // (a W object's callback takes DIDEVICEINSTANCEW)
    let size = if di_wide(c.a(0)) == Some(true) { 0x44c } else { 0x244 };
    let buf = mem::heap_alloc(size, true);
    for kind in [KEYBOARD, MOUSE] {
        let want = match ty & 0xff {
            0 => true,
            1 | 0x12 | 2 => kind == MOUSE && ty & 0xff != 1 || kind == KEYBOARD && ty & 0xff == 1,
            3 | 0x13 => kind == KEYBOARD,
            _ => false,
        };
        if !want {
            continue;
        }
        device_instance(buf, kind, size);
        if c.call(cb, &[buf, refd]) == 0 {
            break;
        }
    }
    mem::heap_free(buf);
    0
}
fn GetDeviceStatus(c: &mut Ctx) -> u32 {
    let k = r32(c.a(1));
    if k == KEYBOARD || k == MOUSE {
        0
    } else {
        0x8007_0002
    }
}
fn FindDevice(_: &mut Ctx) -> u32 {
    0x8007_0002
}

fn kind_of(o: u32) -> u32 {
    devices(|ds| ds.get(&o).map_or(0, |d| d.kind))
}

/// DIDEVCAPS
fn GetCapabilities(c: &mut Ctx) -> u32 {
    let (o, p) = (c.a(0), c.a(1));
    let kind = kind_of(o);
    let size = r32(p).clamp(24, 44);
    mem::fill(p + 4, size - 4, 0);
    w32(p + 4, 5); // attached | emulated
    w32(p + 8, if kind == KEYBOARD { 0x413 } else { 0x212 });
    w32(p + 12, if kind == KEYBOARD { 0 } else { 3 });
    w32(p + 16, if kind == KEYBOARD { 128 } else { 3 });
    0
}
/// DIPROP_BUFFERSIZE (the property GUID pointer 1) in a DIPROPDWORD; others read as 0
fn GetProperty(c: &mut Ctx) -> u32 {
    let (o, prop, p) = (c.a(0), c.a(1), c.a(2));
    if r32(p) >= 20 {
        let v = if prop == 1 { devices(|ds| ds.get(&o).map_or(0, |d| d.cap)) } else { 0 };
        w32(p + 16, v);
    }
    0
}
fn SetProperty(c: &mut Ctx) -> u32 {
    let (o, prop, p) = (c.a(0), c.a(1), c.a(2));
    if prop == 1 && r32(p) >= 20 {
        let n = r32(p + 16).min(4096);
        devices(|ds| {
            if let Some(d) = ds.get_mut(&o) {
                d.cap = n;
                d.buf.truncate(n as usize);
            }
        });
    }
    0
}
/// Acquire: buffered data starts from now
fn Acquire(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    let at = crate::user::cursor_pos();
    devices(|ds| {
        if let Some(d) = ds.get_mut(&o) {
            d.buf.clear();
            d.at = at;
        }
    });
    0
}
/// GetDeviceInfo: the caller's dwSize says which DIDEVICEINSTANCE it has (DIERR_INVALIDPARAM
/// for a size that is none of them, as Windows)
fn GetDeviceInfo(c: &mut Ctx) -> u32 {
    let (o, p) = (c.a(0), c.a(1));
    if p == 0 {
        return 0x8000_4003;
    }
    let size = r32(p);
    if instance_layout(size, devices(|ds| ds.get(&o).and_then(|d| d.wide))).is_none() {
        return 0x8007_0057;
    }
    device_instance(p, kind_of(o), size);
    0
}

/// DirectInput key codes (scan codes; the extended keys + 0x80)
fn dik_of(vk: usize) -> Option<usize> {
    let ext = match vk {
        0x26 => 0xc8,
        0x28 => 0xd0,
        0x25 => 0xcb,
        0x27 => 0xcd,
        0x24 => 0xc7,
        0x23 => 0xcf,
        0x21 => 0xc9,
        0x22 => 0xd1,
        0x2d => 0xd2,
        0x2e => 0xd3,
        0xa3 => 0x9d,
        0xa5 => 0xb8,
        0x6f => 0xb5,
        0x5b => 0xdb,
        0x5c => 0xdc,
        0x5d => 0xdd,
        _ => 0,
    };
    if ext != 0 {
        return Some(ext);
    }
    let s = crate::user::vk_scan(vk as u32) as usize;
    (s != 0 && s < 256).then_some(s)
}

fn GetDeviceState(c: &mut Ctx) -> u32 {
    let (o, n, p) = (c.a(0), c.a(1), c.a(2));
    let keys = crate::user::key_states();
    mem::fill(p, n, 0);
    match kind_of(o) {
        KEYBOARD => {
            for vk in 1..256 {
                if keys[vk] & 0x80 != 0 {
                    if let Some(d) = dik_of(vk) {
                        if (d as u32) < n {
                            mem::w8(p + d as u32, 0x80);
                        }
                    }
                }
            }
            0
        }
        MOUSE => {
            let now = crate::user::cursor_pos();
            let (last, wheel) = devices(|ds| {
                ds.get_mut(&o).map_or((now, 0), |d| (std::mem::replace(&mut d.last, now), std::mem::take(&mut d.wheel)))
            });
            if n >= 12 {
                w32(p, (now.0 - last.0) as u32);
                w32(p + 4, (now.1 - last.1) as u32);
                w32(p + 8, wheel as u32);
            }
            for (i, vk) in [1usize, 2, 4].iter().enumerate() {
                if 12 + i as u32 >= n {
                    break;
                }
                if keys[*vk] & 0x80 != 0 {
                    mem::w8(p + 12 + i as u32, 0x80);
                }
            }
            0
        }
        _ => 0x8007_001e,
    }
}
/// GetDeviceData(cbObjectData, rgdod, pdwInOut, flags): DIDEVICEOBJECTDATA (20 bytes, 16 in
/// DirectX 3's) from the queue; rgdod NULL drops them; DIGDD_PEEK (1) leaves them queued.
fn GetDeviceData(c: &mut Ctx) -> u32 {
    let (o, size, out, inout, flags) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if inout == 0 {
        return 0x8007_0057;
    }
    let want = r32(inout) as usize;
    let got = devices(|ds| {
        let Some(d) = ds.get_mut(&o) else { return None };
        if d.cap == 0 {
            return None;
        }
        let n = want.min(d.buf.len());
        let items: Vec<[u32; 4]> = if flags & 1 != 0 { d.buf.iter().take(n).copied().collect() } else { d.buf.drain(..n).collect() };
        Some(items)
    });
    let Some(items) = got else {
        w32(inout, 0);
        return 0x8004_0207; // DIERR_NOTBUFFERED
    };
    if out != 0 {
        let size = size.max(16);
        for (i, e) in items.iter().enumerate() {
            let p = out + i as u32 * size;
            mem::fill(p, size, 0);
            for (k, v) in e.iter().enumerate() {
                w32(p + 4 * k as u32, *v);
            }
        }
    }
    w32(inout, items.len() as u32);
    0
}

pub fn init() {
    implement("dinput8", "DirectInput8Create", DirectInput8Create);
    implement("dinput", "DirectInputCreateA", DirectInputCreateA);
    implement("dinput", "DirectInputCreateW", DirectInputCreateW);
    implement("dinput", "DirectInputCreateEx", DirectInputCreateEx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_guest::{call, enter};
    use proptest::prelude::*;

    fn device(kind: u32) -> u32 {
        let g = mem::heap_alloc(16, true);
        w32(g, kind);
        let out = mem::heap_alloc(8, true);
        assert_eq!(call(CreateDevice, &[0, g, out, 0]), 0);
        let o = r32(out);
        mem::heap_free(g);
        mem::heap_free(out);
        o
    }

    /// An ANSI device takes only the ANSI sizes, a wide one only the wide ones (as Windows:
    /// DIERR_INVALIDPARAM), one made through CoCreateInstance either
    #[test]
    fn device_info_sizes_follow_the_character_set() {
        for (wide, ok) in [(Some(false), [true, true, false, false]), (Some(true), [false, false, true, true]), (None, [true; 4])] {
            for (size, ok) in [0x230, 0x244, 0x438, 0x44c].into_iter().zip(ok) {
                assert_eq!(instance_layout(size, wide).is_some(), ok, "{size:#x} {wide:?}");
            }
        }
    }

    proptest! {
        /// GetDeviceInfo with any dwSize: the four DIDEVICEINSTANCE sizes are filled up to their
        /// end and no further; any other size is DIERR_INVALIDPARAM and nothing past dwSize changes
        #[test]
        fn device_info_stays_inside_dwsize(size in prop_oneof![Just(0x230u32), Just(0x244), Just(0x438), Just(0x44c), 0u32..0x480], mouse in any::<bool>()) {
            let _g = enter();
            let o = device(if mouse { MOUSE } else { KEYBOARD });
            let p = mem::heap_alloc(0x480, false);
            mem::fill(p, 0x480, 0xaa);
            w32(p, size);
            let r = call(GetDeviceInfo, &[o, p]);
            let valid = instance_layout(size, None).is_some();
            prop_assert_eq!(r, if valid { 0 } else { 0x8007_0057 });
            let end = if valid { size } else { 4 };
            prop_assert!(mem::slice(p + end, 0x480 - end).iter().all(|&b| b == 0xaa), "written past {:#x}", end);
            if valid {
                prop_assert_eq!(r32(p), size);
                prop_assert_eq!(r32(p + 4), if mouse { MOUSE } else { KEYBOARD });
                let name: &[u8] = if mouse { b"Mouse" } else { b"Keyboard" };
                if size >= 0x438 {
                    let w: Vec<u8> = (0..name.len() as u32).map(|i| mem::r16(p + 40 + 2 * i) as u8).collect();
                    prop_assert_eq!(&w[..], name);
                } else {
                    prop_assert_eq!(mem::slice(p + 40, name.len() as u32), name);
                    prop_assert_eq!(mem::slice(p + 300, name.len() as u32), name);
                }
            }
            mem::heap_free(p);
            call(dev_release, &[o]);
        }

        /// Devices created, AddRef'd and Released in any order follow one count each (as
        /// Windows' AddRef/Release return it); the table holds exactly the live ones, so input
        /// is queued only on them, and a released device's block is freed.
        #[test]
        fn devices_count_as_a_model(ops in proptest::collection::vec((0u8..3, any::<prop::sample::Index>()), 0..40)) {
            let _g = enter();
            let before = devices(|ds| ds.len());
            let mut live: Vec<(u32, u32)> = vec![];
            let mut gone = vec![];
            for (op, k) in ops {
                match op {
                    0 => live.push((device(if k.index(2) == 0 { MOUSE } else { KEYBOARD }), 1)),
                    _ if live.is_empty() => {}
                    1 => {
                        let i = k.index(live.len());
                        live[i].1 += 1;
                        prop_assert_eq!(call(dev_add_ref, &[live[i].0]), live[i].1);
                    }
                    _ => {
                        let i = k.index(live.len());
                        live[i].1 -= 1;
                        prop_assert_eq!(call(dev_release, &[live[i].0]), live[i].1);
                        if live[i].1 == 0 {
                            gone.push(live.swap_remove(i).0);
                        }
                    }
                }
                prop_assert_eq!(devices(|ds| ds.len()), before + live.len());
                for &(o, _) in &live {
                    prop_assert!(devices(|ds| ds.contains_key(&o)));
                }
                for &o in &gone {
                    prop_assert!(!devices(|ds| ds.contains_key(&o)) || live.iter().any(|l| l.0 == o));
                }
            }
            for (o, n) in live {
                for _ in 0..n {
                    call(dev_release, &[o]);
                }
                prop_assert!(!mem::heap_owns(o));
            }
            prop_assert_eq!(devices(|ds| ds.len()), before);
        }
    }

    /// A buffered mouse released and made again: the old one is gone (no queue fed), the new
    /// one queues; GetDeviceState on a released (unknown) device is not an abort.
    #[test]
    fn released_devices_get_no_input() {
        let _g = enter();
        let o = device(MOUSE);
        let dp = mem::heap_alloc(20, true);
        for (off, v) in [(0, 20), (4, 16), (16, 8)] {
            w32(dp + off, v);
        }
        assert_eq!(call(SetProperty, &[o, 1, dp]), 0);
        input(4, 0, 0, 120);
        assert_eq!(devices(|ds| ds[&o].buf.len()), 1);
        assert_eq!(call(dev_release, &[o]), 0);
        input(4, 0, 0, 120);
        assert!(!devices(|ds| ds.contains_key(&o)));
        let st = mem::heap_alloc(16, true);
        assert_eq!(call(GetDeviceState, &[o, 16, st]), 0x8007_001e);
        mem::heap_free(st);
        mem::heap_free(dp);
    }
}
