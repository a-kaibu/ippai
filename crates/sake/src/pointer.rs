//! The mouse as a pointer (EnableMouseInPointer, Windows 8+): once a program turns it on, each
//! mouse event also comes as a WM_POINTER* message (pointer id 1, PT_MOUSE) ahead of the usual
//! mouse message (which Windows' DefWindowProc makes from the pointer one), and GetPointerInfo
//! tells the pointer's state. Unity 6's Input System takes the mouse's position and buttons only
//! from these: sake answered EnableMouseInPointer with FALSE, and every Unity 6 game took no
//! click at its title (uGUI's InputSystemUIInputModule saw a mouse that never moved or pressed).
//!
//! The message for an event and POINTER_INFO's bytes are pure functions here; user.rs posts them.
#![allow(non_snake_case)]
use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem::{self, w32};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

pub const WM_POINTERUPDATE: u32 = 0x245;
pub const WM_POINTERDOWN: u32 = 0x246;
pub const WM_POINTERUP: u32 = 0x247;
pub const WM_POINTERWHEEL: u32 = 0x24e;
/// The mouse's pointer id (Windows numbers it 1)
pub const MOUSE_ID: u32 = 1;
pub const PT_MOUSE: u32 = 4;

pub const FLAG_INRANGE: u32 = 0x2;
pub const FLAG_INCONTACT: u32 = 0x4;
pub const FLAG_FIRSTBUTTON: u32 = 0x10;
pub const FLAG_SECONDBUTTON: u32 = 0x20;
pub const FLAG_THIRDBUTTON: u32 = 0x40;
pub const FLAG_PRIMARY: u32 = 0x2000;
pub const FLAG_CONFIDENCE: u32 = 0x4000;
pub const FLAG_DOWN: u32 = 0x1_0000;
pub const FLAG_UPDATE: u32 = 0x2_0000;
pub const FLAG_UP: u32 = 0x4_0000;
pub const FLAG_WHEEL: u32 = 0x8_0000;

/// Mouse buttons as a bit set: left 1, right 2, middle 4 (the order of the page's button numbers)
pub type Buttons = u8;

/// 0: never set, 1: off, 2: on (EnableMouseInPointer takes one answer per process)
static MODE: AtomicU8 = AtomicU8::new(0);

pub fn on() -> bool {
    MODE.load(Ordering::Relaxed) == 2
}

/// A pointer message and what GetPointerInfo then tells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Event {
    pub msg: u32,
    pub wparam: u32,
    /// POINTER_FLAGS (the message's flags in the low 16 bits, and DOWN / UPDATE / UP / WHEEL)
    pub flags: u32,
    /// POINTER_BUTTON_CHANGE_TYPE
    pub change: u32,
    /// the wheel's delta (InputData)
    pub data: i32,
}

/// The pointer message of a mouse event: `kind` 1 a move, 2 button `button` (0 left, 1 right,
/// 2 middle) down, 3 up, 4 the wheel by `wheel`; `before` / `after` the buttons held. Windows
/// sends WM_POINTERDOWN for the first button to go down and WM_POINTERUP for the last to come
/// up; a button in between is a WM_POINTERUPDATE with the change. A mouse is always in range,
/// in contact while a button is held, and the primary pointer.
pub fn event(kind: u32, button: u32, wheel: i32, before: Buttons, after: Buttons) -> Option<Event> {
    let held = |b: Buttons| {
        let mut f = FLAG_INRANGE | FLAG_PRIMARY | FLAG_CONFIDENCE;
        for (bit, flag) in [(1, FLAG_FIRSTBUTTON), (2, FLAG_SECONDBUTTON), (4, FLAG_THIRDBUTTON)] {
            if b & bit != 0 {
                f |= flag;
            }
        }
        if b != 0 {
            f |= FLAG_INCONTACT;
        }
        f
    };
    let b = button.min(2);
    // (a button that was down already, or up already, changes nothing: a move)
    let changed = before ^ after != 0;
    let (msg, change, flags) = match kind {
        1 => (WM_POINTERUPDATE, 0, held(after) | FLAG_UPDATE),
        2 | 3 if !changed => (WM_POINTERUPDATE, 0, held(after) | FLAG_UPDATE),
        2 if before == 0 => (WM_POINTERDOWN, 1 + 2 * b, held(after) | FLAG_DOWN),
        2 => (WM_POINTERUPDATE, 1 + 2 * b, held(after) | FLAG_UPDATE),
        3 if after == 0 => (WM_POINTERUP, 2 + 2 * b, held(after) | FLAG_UP),
        3 => (WM_POINTERUPDATE, 2 + 2 * b, held(after) | FLAG_UPDATE),
        4 => (WM_POINTERWHEEL, 0, held(after) | FLAG_WHEEL),
        _ => return None,
    };
    // wParam: the pointer id, and the message's flags (WM_POINTERWHEEL: the wheel's delta)
    let high = if msg == WM_POINTERWHEEL { wheel as u16 as u32 } else { flags & 0xffff };
    Some(Event { msg, wparam: MOUSE_ID | high << 16, flags, change, data: if msg == WM_POINTERWHEEL { wheel } else { 0 } })
}

/// The pointer's last state, as GetPointerInfo answers it
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Info {
    pub event: Event,
    pub frame: u32,
    pub hwnd: u32,
    /// screen pixels
    pub x: i32,
    pub y: i32,
    pub time: u32,
    /// POINTER_MOD_SHIFT (4) / POINTER_MOD_CTRL (8)
    pub keys: u32,
    pub qpc: u64,
}

static LAST: Mutex<Option<Info>> = Mutex::new(None);

pub fn set_last(i: Info) {
    *LAST.lock().unwrap() = Some(i);
}

/// POINTER_INFO's bytes: 88 on x86, 96 on x64 (sourceDevice and hwndTarget are pointer-sized).
pub fn info_bytes(i: &Info, x64: bool) -> Vec<u8> {
    let mut b = Vec::with_capacity(96);
    let u = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
    let p = |b: &mut Vec<u8>, v: u32| {
        b.extend_from_slice(&v.to_le_bytes());
        if x64 {
            b.extend_from_slice(&[0; 4]);
        }
    };
    u(&mut b, PT_MOUSE);
    u(&mut b, MOUSE_ID);
    u(&mut b, i.frame);
    u(&mut b, i.event.flags);
    p(&mut b, 0); // sourceDevice
    p(&mut b, i.hwnd);
    // ptPixelLocation, ptHimetricLocation (the mouse has no digitizer: pixels again), and their
    // raw (unpredicted) forms
    for _ in 0..4 {
        u(&mut b, i.x as u32);
        u(&mut b, i.y as u32);
    }
    u(&mut b, i.time);
    u(&mut b, 1); // historyCount
    u(&mut b, i.event.data as u32);
    u(&mut b, i.keys);
    b.extend_from_slice(&i.qpc.to_le_bytes());
    u(&mut b, i.event.change);
    u(&mut b, 0); // (padding to 8)
    b
}

/// EnableMouseInPointer(enable): once per process; the opposite answer later is refused
fn EnableMouseInPointer(c: &mut Ctx) -> u32 {
    let want = if c.a(0) != 0 { 2 } else { 1 };
    match MODE.compare_exchange(0, want, Ordering::Relaxed, Ordering::Relaxed) {
        Ok(_) => 1,
        Err(m) if m == want => 1,
        Err(_) => {
            last_error(5);
            0
        }
    }
}
fn IsMouseInPointerEnabled(_: &mut Ctx) -> u32 {
    on() as u32
}

/// The mouse is the only pointer, and one only while it is in pointer mode
fn known(id: u32) -> bool {
    if id == MOUSE_ID && on() {
        return true;
    }
    last_error(87);
    false
}

/// GetPointerType(id, &type)
fn GetPointerType(c: &mut Ctx) -> u32 {
    if !known(c.a(0) & 0xffff) || c.a(1) == 0 {
        return 0;
    }
    w32(c.a(1), PT_MOUSE);
    1
}

/// GetPointerInfo(id, POINTER_INFO*)
fn GetPointerInfo(c: &mut Ctx) -> u32 {
    if !known(c.a(0) & 0xffff) || c.a(1) == 0 {
        return 0;
    }
    let i = LAST.lock().unwrap().unwrap_or_default();
    mem::write(c.a(1), &info_bytes(&i, mem::x64()));
    1
}

/// GetPointerDevices(&count, devices): no touch or pen digitizers (a mouse is none)
fn GetPointerDevices(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        last_error(87);
        return 0;
    }
    w32(c.a(0), 0);
    1
}

pub fn init() {
    for (n, f) in [
        ("EnableMouseInPointer", EnableMouseInPointer as crate::api::Handler),
        ("IsMouseInPointerEnabled", IsMouseInPointerEnabled),
        ("GetPointerType", GetPointerType),
        ("GetPointerInfo", GetPointerInfo),
        ("GetPointerDevices", GetPointerDevices),
    ] {
        implement("user32", n, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_click() {
        let down = event(2, 0, 0, 0, 1).unwrap();
        assert_eq!(down.msg, WM_POINTERDOWN);
        assert_eq!(down.wparam, 1 | (FLAG_INRANGE | FLAG_INCONTACT | FLAG_FIRSTBUTTON | FLAG_PRIMARY | FLAG_CONFIDENCE) << 16);
        assert_eq!(down.change, 1);
        let up = event(3, 0, 0, 1, 0).unwrap();
        assert_eq!(up.msg, WM_POINTERUP);
        assert_eq!(up.wparam >> 16, FLAG_INRANGE | FLAG_PRIMARY | FLAG_CONFIDENCE);
        assert_eq!((up.change, up.flags & FLAG_UP), (2, FLAG_UP));
        // the right button while the left is held: an update with the change
        let r = event(2, 1, 0, 1, 3).unwrap();
        assert_eq!((r.msg, r.change), (WM_POINTERUPDATE, 3));
        let w = event(4, 0, -120, 0, 0).unwrap();
        assert_eq!((w.msg, w.wparam, w.data), (WM_POINTERWHEEL, 1 | 0xff88 << 16, -120));
        assert_eq!(event(7, 0, 0, 0, 0), None);
    }

    #[test]
    fn layouts() {
        let i = Info { event: event(2, 0, 0, 0, 1).unwrap(), frame: 7, hwnd: 0x10020, x: 682, y: 356, time: 1234, keys: 8, qpc: 0x1_0000_0002 };
        let b64 = info_bytes(&i, true);
        let b32 = info_bytes(&i, false);
        assert_eq!((b64.len(), b32.len()), (96, 88));
        let at = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        // x64: hwndTarget 24, ptPixelLocation 32, dwTime 64, InputData 72, dwKeyStates 76,
        // PerformanceCount 80, ButtonChangeType 88
        assert_eq!((at(&b64, 0), at(&b64, 4), at(&b64, 8), at(&b64, 24), at(&b64, 32), at(&b64, 36)), (4, 1, 7, 0x10020, 682, 356));
        assert_eq!((at(&b64, 64), at(&b64, 76), at(&b64, 80), at(&b64, 84), at(&b64, 88)), (1234, 8, 2, 1, 1));
        // x86: hwndTarget 20, ptPixelLocation 24, dwTime 56, PerformanceCount 72, ButtonChangeType 80
        assert_eq!((at(&b32, 20), at(&b32, 24), at(&b32, 56), at(&b32, 72), at(&b32, 80)), (0x10020, 682, 1234, 2, 1));
    }

    fn step() -> impl Strategy<Value = (u32, u32, i32)> {
        (1u32..=4, 0u32..3, -480i32..480)
    }

    proptest! {
        /// Over any run of mouse events: one WM_POINTERDOWN opens and one WM_POINTERUP closes
        /// each stretch of held buttons, the flags always say which buttons are held, the
        /// wParam carries the id and the flags (or the wheel), and a change names the button.
        #[test]
        fn messages_follow_the_buttons(steps in proptest::collection::vec(step(), 0..80)) {
            let mut held: Buttons = 0;
            let mut open = false;
            for (kind, button, wheel) in steps {
                let bit = 1u8 << button;
                let after = match kind { 2 => held | bit, 3 => held & !bit, _ => held };
                let e = event(kind, button, wheel, held, after).unwrap();
                prop_assert_eq!(e.wparam & 0xffff, MOUSE_ID);
                for (b, f) in [(1, FLAG_FIRSTBUTTON), (2, FLAG_SECONDBUTTON), (4, FLAG_THIRDBUTTON)] {
                    prop_assert_eq!(after & b != 0, e.flags & f != 0);
                }
                prop_assert_eq!(after != 0, e.flags & FLAG_INCONTACT != 0);
                prop_assert!(e.flags & (FLAG_INRANGE | FLAG_PRIMARY) == FLAG_INRANGE | FLAG_PRIMARY);
                if e.msg == WM_POINTERWHEEL {
                    prop_assert_eq!((e.wparam >> 16) as u16 as i16 as i32, wheel);
                } else {
                    prop_assert_eq!(e.wparam >> 16, e.flags & 0xffff);
                }
                match e.msg {
                    WM_POINTERDOWN => { prop_assert!(!open); open = true; }
                    WM_POINTERUP => { prop_assert!(open); open = false; }
                    _ => {}
                }
                prop_assert_eq!(open, after != 0);
                match kind {
                    2 if held != after => prop_assert_eq!(e.change, 1 + 2 * button),
                    3 if held != after => prop_assert_eq!(e.change, 2 + 2 * button),
                    _ => prop_assert_eq!(e.change, 0),
                }
                held = after;
            }
        }
    }
}
