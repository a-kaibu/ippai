//! dinput8.dll / dinput.dll: the system keyboard and mouse as DirectInput devices, read from
//! user32's key state and cursor. Joysticks are not there (EnumDevices lists none).

#![allow(non_snake_case)]

use crate::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::api::{implement, Ctx, Handler};
use crate::mem::{self, put32, r32, w32};

const KEYBOARD: u32 = 0x6F1D_2B61;
const MOUSE: u32 = 0x6F1D_2B60;

use crate::api::vtable;

/// device object -> (kind, last cursor for relative mouse motion)
static DEVICES: Mutex<Option<HashMap<u32, (u32, (i32, i32))>>> = Mutex::new(None);

fn ok(_: &mut Ctx) -> u32 {
    0
}
fn add_ref(_: &mut Ctx) -> u32 {
    1
}
fn release(_: &mut Ctx) -> u32 {
    0
}
fn no_interface(c: &mut Ctx) -> u32 {
    put32(c.a(2), 0);
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
            (add_ref, 4),
            (release, 4),
            (GetCapabilities, 8),
            (ok, 20),
            (GetProperty, 12),
            (ok, 12),
            (ok, 4),
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

fn create_di(out: u32, v7: bool) -> u32 {
    let o = mem::heap_alloc(8, true);
    w32(o, di_vtbl(v7));
    put32(out, o);
    0
}

fn DirectInput8Create(c: &mut Ctx) -> u32 {
    create_di(c.a(3), false)
}
fn DirectInputCreateA(c: &mut Ctx) -> u32 {
    create_di(c.a(2), true)
}
fn DirectInputCreateEx(c: &mut Ctx) -> u32 {
    create_di(c.a(3), true)
}

/// CoCreateInstance of CLSID_DirectInput / CLSID_DirectInput8 (then Initialize).
pub fn create_instance(clsid: u32) -> Option<u32> {
    let v7 = match mem::r32(clsid) {
        0x25e6_09e0 => true,
        0x25e6_09e4 => false,
        _ => return None,
    };
    let out = mem::heap_alloc(4, true);
    create_di(out, v7);
    let o = mem::r32(out);
    mem::heap_free(out);
    Some(o)
}

fn CreateDeviceEx(c: &mut Ctx) -> u32 {
    let (guid, out) = (c.a(1), c.a(3));
    create_device(guid, out)
}

fn CreateDevice(c: &mut Ctx) -> u32 {
    create_device(c.a(1), c.a(2))
}
fn create_device(guid: u32, out: u32) -> u32 {
    let kind = r32(guid);
    if kind != KEYBOARD && kind != MOUSE {
        put32(out, 0);
        return 0x8007_0002;
    }
    let o = mem::heap_alloc(8, true);
    w32(o, dev_vtbl());
    DEVICES.lock().unwrap().get_or_insert_with(HashMap::default).insert(o, (kind, crate::user::cursor_pos()));
    put32(out, o);
    0
}

fn write_guid(p: u32, first: u32) {
    w32(p, first);
    mem::write(p + 4, &[0xa0, 0xd5, 0xcf, 0x11, 0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0, 0]);
}

/// DIDEVICEINSTANCEA
fn device_instance(p: u32, kind: u32) {
    let size = r32(p).max(0x244).min(0x244);
    mem::fill(p, size, 0);
    w32(p, size);
    write_guid(p + 4, kind);
    write_guid(p + 20, kind);
    w32(p + 36, if kind == KEYBOARD { 0x413 } else { 0x212 });
    let name: &[u8] = if kind == KEYBOARD { b"Keyboard" } else { b"Mouse" };
    mem::write(p + 40, name);
    mem::write(p + 300, name);
}

fn EnumDevices(c: &mut Ctx) -> u32 {
    let (ty, cb, refd) = (c.a(1), c.a(2), c.a(3));
    let buf = mem::heap_alloc(0x244, true);
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
        w32(buf, 0x244);
        device_instance(buf, kind);
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
    DEVICES.lock().unwrap().get_or_insert_with(HashMap::default).get(&o).map_or(0, |d| d.0)
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
fn GetProperty(c: &mut Ctx) -> u32 {
    // DIPROPDWORD: buffer size and the like read as 0
    let p = c.a(2);
    if r32(p) >= 20 {
        w32(p + 16, 0);
    }
    0
}
fn GetDeviceInfo(c: &mut Ctx) -> u32 {
    let (o, p) = (c.a(0), c.a(1));
    device_instance(p, kind_of(o));
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
            let last = {
                let mut g = DEVICES.lock().unwrap();
                let d = g.get_or_insert_with(HashMap::default).get_mut(&o).unwrap();
                std::mem::replace(&mut d.1, now)
            };
            if n >= 12 {
                w32(p, (now.0 - last.0) as u32);
                w32(p + 4, (now.1 - last.1) as u32);
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
/// buffered data: nothing queued
fn GetDeviceData(c: &mut Ctx) -> u32 {
    put32(c.a(3), 0);
    0
}

pub fn init() {
    implement("dinput8", "DirectInput8Create", DirectInput8Create);
    implement("dinput", "DirectInputCreateA", DirectInputCreateA);
    implement("dinput", "DirectInputCreateW", DirectInputCreateA);
    implement("dinput", "DirectInputCreateEx", DirectInputCreateEx);
}
