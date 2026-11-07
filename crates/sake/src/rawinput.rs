//! Raw Input (user32): one mouse and one keyboard. A window that registers for them gets
//! WM_INPUT with each mouse and key event (alongside the usual messages, unless it asked for
//! RIDEV_NOLEGACY), and reads the event with GetRawInputData. Unity 2019+ fails its input
//! initialization (and then crashes) when the registration fails.
#![allow(non_snake_case)]
use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem::{self, r16, r32, w32};
use std::collections::VecDeque;
use std::sync::Mutex;

pub const MOUSE: u32 = 0x0001_0041;
pub const KEYBOARD: u32 = 0x0001_0043;
const RIM_TYPEMOUSE: u32 = 0;
const RIM_TYPEKEYBOARD: u32 = 1;
const RIDEV_REMOVE: u32 = 1;
const RIDEV_NOLEGACY: u32 = 0x30;
pub const WM_INPUT: u32 = 0xff;

/// (usage page, usage, flags, target window) of each registration
static REGS: Mutex<Vec<(u16, u16, u32, u32)>> = Mutex::new(Vec::new());
/// The last events (handle, RAWINPUT): a WM_INPUT's lParam stays readable while its message
/// waits in the queue
static EVENTS: Mutex<(u32, VecDeque<(u32, Vec<u8>)>)> = Mutex::new((0, VecDeque::new()));
const KEPT: usize = 1024;

fn header_size() -> u32 {
    8 + 2 * mem::ps()
}

/// The registration for the mouse (usage 2) or the keyboard (6) on the generic desktop page:
/// (target window, 0 for the focus; whether legacy messages are off)
pub fn registered(mouse: bool) -> Option<(u32, bool)> {
    let usage = if mouse { 2 } else { 6 };
    REGS.lock()
        .unwrap()
        .iter()
        .find(|r| r.0 == 1 && r.1 == usage)
        .map(|r| (r.3, r.2 & RIDEV_NOLEGACY == RIDEV_NOLEGACY))
}

fn record(ty: u32, device: u32, body: &[u8]) -> u32 {
    let hs = header_size();
    let mut b = Vec::with_capacity(hs as usize + body.len());
    b.extend_from_slice(&ty.to_le_bytes());
    b.extend_from_slice(&(hs + body.len() as u32).to_le_bytes());
    b.extend_from_slice(&device.to_le_bytes());
    if mem::x64() {
        b.extend_from_slice(&[0; 4]);
    }
    b.extend_from_slice(&[0; 8][..mem::ps() as usize]); // wParam: RIM_INPUT
    b.extend_from_slice(body);
    let mut g = EVENTS.lock().unwrap();
    g.0 = g.0.wrapping_add(4) | 0x4000_0000;
    let h = g.0;
    g.1.push_back((h, b));
    if g.1.len() > KEPT {
        g.1.pop_front();
    }
    h
}

/// A mouse event: RI_MOUSE_* button flags, wheel delta, relative motion
pub fn mouse_event(buttons: u16, wheel: i16, dx: i32, dy: i32) -> u32 {
    let mut m = Vec::with_capacity(24);
    m.extend_from_slice(&0u16.to_le_bytes()); // usFlags: MOUSE_MOVE_RELATIVE
    m.extend_from_slice(&0u16.to_le_bytes());
    m.extend_from_slice(&buttons.to_le_bytes());
    m.extend_from_slice(&(wheel as u16).to_le_bytes());
    m.extend_from_slice(&0u32.to_le_bytes()); // ulRawButtons
    m.extend_from_slice(&dx.to_le_bytes());
    m.extend_from_slice(&dy.to_le_bytes());
    m.extend_from_slice(&0u32.to_le_bytes());
    record(RIM_TYPEMOUSE, MOUSE, &m)
}

/// A key event: scan code, RI_KEY_* flags, virtual key, the WM_KEY* message
pub fn key_event(scan: u16, flags: u16, vk: u16, msg: u32) -> u32 {
    let mut k = Vec::with_capacity(16);
    for v in [scan, flags, 0, vk] {
        k.extend_from_slice(&v.to_le_bytes());
    }
    k.extend_from_slice(&msg.to_le_bytes());
    k.extend_from_slice(&0u32.to_le_bytes());
    record(RIM_TYPEKEYBOARD, KEYBOARD, &k)
}

/// RegisterRawInputDevices(devices, n, size)
fn RegisterRawInputDevices(c: &mut Ctx) -> u32 {
    let (p, n, size) = (c.a(0), c.a(1), c.a(2));
    if size != 8 + mem::ps() || p == 0 || n == 0 {
        last_error(87);
        return 0;
    }
    let mut regs = REGS.lock().unwrap();
    for i in 0..n {
        let d = p + i * size;
        let (page, usage, flags, hwnd) = (r16(d), r16(d + 2), r32(d + 4), mem::rp(d + 8));
        regs.retain(|r| (r.0, r.1) != (page, usage));
        if flags & RIDEV_REMOVE == 0 {
            regs.push((page, usage, flags, hwnd));
        }
    }
    1
}

/// GetRegisteredRawInputDevices(devices, &n, size)
fn GetRegisteredRawInputDevices(c: &mut Ctx) -> u32 {
    let (p, pn, size) = (c.a(0), c.a(1), c.a(2));
    if size != 8 + mem::ps() || pn == 0 {
        last_error(87);
        return u32::MAX;
    }
    let regs = REGS.lock().unwrap();
    let have = regs.len() as u32;
    if p == 0 || r32(pn) < have {
        w32(pn, have);
        if p == 0 {
            return 0;
        }
        last_error(122);
        return u32::MAX;
    }
    for (i, r) in regs.iter().enumerate() {
        let d = p + i as u32 * size;
        mem::w16(d, r.0);
        mem::w16(d + 2, r.1);
        w32(d + 4, r.2);
        mem::wp(d + 8, r.3);
    }
    have
}

/// GetRawInputDeviceList(list, &n, size): the mouse and the keyboard
fn GetRawInputDeviceList(c: &mut Ctx) -> u32 {
    let (p, pn, size) = (c.a(0), c.a(1), c.a(2));
    if size != 2 * mem::ps() || pn == 0 {
        last_error(87);
        return u32::MAX;
    }
    if p == 0 {
        w32(pn, 2);
        return 0;
    }
    if r32(pn) < 2 {
        w32(pn, 2);
        last_error(122);
        return u32::MAX;
    }
    for (i, (h, t)) in [(MOUSE, RIM_TYPEMOUSE), (KEYBOARD, RIM_TYPEKEYBOARD)]
        .into_iter()
        .enumerate()
    {
        let d = p + i as u32 * size;
        mem::wp(d, h);
        w32(d + mem::ps(), t);
    }
    2
}

/// GetRawInputDeviceInfo(device, command, data, &size): its name, RID_DEVICE_INFO, no HID
/// preparsed data
fn device_info(c: &mut Ctx, wide: bool) -> u32 {
    let (h, cmd, p, ps) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if (h != MOUSE && h != KEYBOARD) || ps == 0 {
        last_error(6);
        return u32::MAX;
    }
    let mouse = h == MOUSE;
    let (data, unit): (Vec<u8>, u32) = match cmd {
        0x2000_0007 => {
            // RIDI_DEVICENAME: in characters, with the terminating 0
            let name = if mouse {
                "\\\\?\\HID#VID_046D&PID_C077#7&1a2b3c4d&0&0000#{378de44c-56ef-11d1-bc8c-00a0c91405dd}"
            } else {
                "\\\\?\\HID#VID_046D&PID_C31C&MI_00#7&2b3c4d5e&0&0000#{884b96c3-56ef-11d1-bc8c-00a0c91405dd}"
            };
            if wide {
                (
                    name.encode_utf16()
                        .chain([0])
                        .flat_map(|u| u.to_le_bytes())
                        .collect(),
                    2,
                )
            } else {
                (name.bytes().chain([0]).collect(), 1)
            }
        }
        0x2000_000b => {
            // RIDI_DEVICEINFO: RID_DEVICE_INFO (32 bytes)
            let v: [u32; 8] = if mouse {
                [32, RIM_TYPEMOUSE, 2, 5, 60, 0, 0, 0]
            } else {
                [32, RIM_TYPEKEYBOARD, 4, 0, 1, 12, 3, 101]
            };
            (v.iter().flat_map(|x| x.to_le_bytes()).collect(), 1)
        }
        0x2000_0005 => (Vec::new(), 1), // RIDI_PREPARSEDDATA: none (not a HID device here)
        _ => {
            last_error(87);
            return u32::MAX;
        }
    };
    let n = data.len() as u32 / unit;
    if cmd == 0x2000_000b && p != 0 && r32(p) != 32 {
        // (the caller sets cbSize)
        last_error(87);
        return u32::MAX;
    }
    if p == 0 {
        w32(ps, n);
        return 0;
    }
    if r32(ps) < n {
        w32(ps, n);
        last_error(122);
        return u32::MAX;
    }
    mem::write(p, &data);
    n
}
fn GetRawInputDeviceInfoA(c: &mut Ctx) -> u32 {
    device_info(c, false)
}
fn GetRawInputDeviceInfoW(c: &mut Ctx) -> u32 {
    device_info(c, true)
}

/// GetRawInputData(event, RID_INPUT / RID_HEADER, data, &size, header size)
fn GetRawInputData(c: &mut Ctx) -> u32 {
    let (h, cmd, p, ps, hsz) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if hsz != header_size() || ps == 0 || (cmd != 0x1000_0003 && cmd != 0x1000_0005) {
        last_error(87);
        return u32::MAX;
    }
    let g = EVENTS.lock().unwrap();
    let Some((_, b)) = g.1.iter().find(|(x, _)| *x == h) else {
        last_error(6);
        return u32::MAX;
    };
    let n = if cmd == 0x1000_0005 {
        hsz
    } else {
        b.len() as u32
    };
    if p == 0 {
        w32(ps, n);
        return 0;
    }
    if r32(ps) < n {
        w32(ps, n);
        last_error(122);
        return u32::MAX;
    }
    mem::write(p, &b[..n as usize]);
    n
}

/// GetRawInputBuffer: events come one WM_INPUT at a time here, none are buffered
fn GetRawInputBuffer(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 || c.a(2) != header_size() {
        last_error(87);
        return u32::MAX;
    }
    if c.a(0) == 0 {
        w32(c.a(1), 0);
    }
    0
}

pub fn init() {
    for (n, f) in [
        (
            "RegisterRawInputDevices",
            RegisterRawInputDevices as crate::api::Handler,
        ),
        ("GetRegisteredRawInputDevices", GetRegisteredRawInputDevices),
        ("GetRawInputDeviceList", GetRawInputDeviceList),
        ("GetRawInputDeviceInfoA", GetRawInputDeviceInfoA),
        ("GetRawInputDeviceInfoW", GetRawInputDeviceInfoW),
        ("GetRawInputData", GetRawInputData),
        ("GetRawInputBuffer", GetRawInputBuffer),
    ] {
        implement("user32", n, f);
    }
}
