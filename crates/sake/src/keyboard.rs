//! The keyboard layouts sake answers with: the US 101-key one (kbdus) and the Japanese 106/109-key
//! one (kbd106), as tables of scan code -> virtual key and virtual key -> characters (unshifted,
//! shifted, with Ctrl), and the user32 calls that read them: MapVirtualKey(Ex)A/W (all five
//! MAPVK_ types), ToUnicode(Ex), ToAscii(Ex), VkKeyScan(Ex)A/W. Neither layout has dead keys or
//! AltGr characters. (Ren'Py's SDL calls MapVirtualKeyExA and ToUnicode on every key; with no
//! answer it saw no text input.) tests/winref/probes/keymap.c makes the same calls on Windows.

#![allow(non_snake_case)]

use crate::api::Ctx;
use crate::mem;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    Us,
    Ja,
}

/// The layout an HKL names (0: the thread's, GetKeyboardLayout's).
pub fn layout_of(hkl: u32) -> Layout {
    let k = if hkl == 0 { crate::user::keyboard_layout() } else { hkl };
    if k & 0xffff == 0x411 { Layout::Ja } else { Layout::Us }
}

const SHIFT: u32 = 0x10;
const CONTROL: u32 = 0x11;
const MENU: u32 = 0x12;

/// Scan code (no E0 prefix) -> virtual key, the keys both layouts share.
fn base_vk(scan: u32) -> u32 {
    const LETTERS: &[u8] = b"QWERTYUIOP\0\0\0\0ASDFGHJKL\0\0\0\0\0ZXCVBNM";
    match scan {
        0x01 => 0x1b,
        0x02..=0x0a => 0x30 + scan - 1,
        0x0b => 0x30,
        0x0c => 0xbd,
        0x0e => 0x08,
        0x0f => 0x09,
        0x10..=0x32 if LETTERS[(scan - 0x10) as usize] != 0 => LETTERS[(scan - 0x10) as usize] as u32,
        0x1c => 0x0d,
        0x1d => 0xa2,
        0x2a => 0xa0,
        0x33 => 0xbc,
        0x34 => 0xbe,
        0x35 => 0xbf,
        0x36 => 0xa1,
        0x37 => 0x6a,
        0x38 => 0xa4,
        0x39 => 0x20,
        0x3a => 0x14,
        0x3b..=0x44 => 0x70 + scan - 0x3b,
        0x45 => 0x90,
        0x46 => 0x91,
        // the numeric keypad without NumLock
        0x47 => 0x24,
        0x48 => 0x26,
        0x49 => 0x21,
        0x4a => 0x6d,
        0x4b => 0x25,
        0x4c => 0x0c,
        0x4d => 0x27,
        0x4e => 0x6b,
        0x4f => 0x23,
        0x50 => 0x28,
        0x51 => 0x22,
        0x52 => 0x2d,
        0x53 => 0x2e,
        0x54 => 0x2c,
        0x57 => 0x7a,
        0x58 => 0x7b,
        0x64..=0x6e => 0x7c + scan - 0x64,
        0x76 => 0x87,
        _ => 0,
    }
}

/// Scan code (no prefix) -> virtual key in `l` (0: none).
pub fn scan_vk(l: Layout, scan: u32) -> u32 {
    let own = match (l, scan) {
        (Layout::Us, 0x0d) => 0xbb,
        (Layout::Us, 0x1a) => 0xdb,
        (Layout::Us, 0x1b) => 0xdd,
        (Layout::Us, 0x27) => 0xba,
        (Layout::Us, 0x28) => 0xde,
        (Layout::Us, 0x29) => 0xc0,
        (Layout::Us, 0x2b) => 0xdc,
        (Layout::Us, 0x56) => 0xe2,
        (Layout::Ja, 0x0d) => 0xde,
        (Layout::Ja, 0x1a) => 0xc0,
        (Layout::Ja, 0x1b) => 0xdb,
        (Layout::Ja, 0x27) => 0xbb,
        (Layout::Ja, 0x28) => 0xba,
        // 半角/全角 (VK_OEM_AUTO), カタカナ/ひらがな (VK_OEM_COPY), ろ, 変換, 無変換, ¥
        (Layout::Ja, 0x29) => 0xf3,
        (Layout::Ja, 0x2b) => 0xdd,
        (Layout::Ja, 0x70) => 0xf2,
        (Layout::Ja, 0x73) => 0xe2,
        (Layout::Ja, 0x79) => 0x1c,
        (Layout::Ja, 0x7b) => 0x1d,
        (Layout::Ja, 0x7d) => 0xdc,
        _ => 0,
    };
    if own != 0 { own } else { base_vk(scan) }
}

/// E0-prefixed scan code -> virtual key.
fn ext_vk(scan: u32) -> u32 {
    match scan {
        0x1c => 0x0d,
        0x1d => 0xa3,
        0x35 => 0x6f,
        0x37 => 0x2c,
        0x38 => 0xa5,
        0x45 => 0x90,
        0x47 => 0x24,
        0x48 => 0x26,
        0x49 => 0x21,
        0x4b => 0x25,
        0x4d => 0x27,
        0x4f => 0x23,
        0x50 => 0x28,
        0x51 => 0x22,
        0x52 => 0x2d,
        0x53 => 0x2e,
        0x5b => 0x5b,
        0x5c => 0x5c,
        0x5d => 0x5d,
        _ => 0,
    }
}

/// Virtual key -> (scan code, extended) in `l`; (0, false) when no key makes it.
pub fn vk_scan(l: Layout, vk: u32) -> (u32, bool) {
    let s = match vk {
        SHIFT | 0xa0 => return (0x2a, false),
        0xa1 => return (0x36, false),
        CONTROL | 0xa2 => return (0x1d, false),
        0xa3 => return (0x1d, true),
        MENU | 0xa4 => return (0x38, false),
        0xa5 => return (0x38, true),
        0x24 => return (0x47, true),
        0x26 => return (0x48, true),
        0x21 => return (0x49, true),
        0x25 => return (0x4b, true),
        0x27 => return (0x4d, true),
        0x23 => return (0x4f, true),
        0x28 => return (0x50, true),
        0x22 => return (0x51, true),
        0x2d => return (0x52, true),
        0x2e => return (0x53, true),
        0x6f => return (0x35, true),
        0x90 => return (0x45, true),
        0x5b | 0x5c | 0x5d => return (vk, true),
        0x2c => return (0x54, false),
        // the keypad's digits and point (NumLock on): their keys
        0x60..=0x69 => [0x52, 0x4f, 0x50, 0x51, 0x4b, 0x4c, 0x4d, 0x47, 0x48, 0x49][(vk - 0x60) as usize],
        0x6e => 0x53,
        _ => (1..0x80).find(|&s| scan_vk(l, s) == vk).unwrap_or(0),
    };
    (s, false)
}

/// The characters of a key: (unshifted, shifted, with Ctrl) in `l`; 0: none.
fn chars(l: Layout, vk: u32) -> (u32, u32, u32) {
    match vk {
        0x41..=0x5a => (vk + 0x20, vk, vk - 0x40),
        0x30..=0x39 => {
            let shifted = match l {
                Layout::Us => b")!@#$%^&*("[(vk - 0x30) as usize],
                Layout::Ja => b"\0!\"#$%&'()"[(vk - 0x30) as usize],
            };
            (vk, shifted as u32, 0)
        }
        0x08 => (8, 8, 0x7f),
        0x09 => (9, 9, 0),
        0x0d => (0x0d, 0x0d, 0x0a),
        0x1b => (0x1b, 0x1b, 0x1b),
        0x20 => (0x20, 0x20, 0x20),
        0x03 => (3, 3, 3),
        0x60..=0x69 => (vk - 0x30, vk - 0x30, 0),
        0x6a => (b'*' as u32, b'*' as u32, 0),
        0x6b => (b'+' as u32, b'+' as u32, 0),
        0x6d => (b'-' as u32, b'-' as u32, 0),
        0x6e => (b'.' as u32, b'.' as u32, 0),
        0x6f => (b'/' as u32, b'/' as u32, 0),
        _ => {
            const US: [(u32, u8, u8, u8); 12] = [
                (0xba, b';', b':', 0), (0xbb, b'=', b'+', 0), (0xbc, b',', b'<', 0), (0xbd, b'-', b'_', 0),
                (0xbe, b'.', b'>', 0), (0xbf, b'/', b'?', 0), (0xc0, b'`', b'~', 0), (0xdb, b'[', b'{', 0x1b),
                (0xdc, b'\\', b'|', 0x1c), (0xdd, b']', b'}', 0x1d), (0xde, b'\'', b'"', 0), (0xe2, b'\\', b'|', 0x1c),
            ];
            const JA: [(u32, u8, u8, u8); 12] = [
                (0xba, b':', b'*', 0), (0xbb, b';', b'+', 0), (0xbc, b',', b'<', 0), (0xbd, b'-', b'=', 0),
                (0xbe, b'.', b'>', 0), (0xbf, b'/', b'?', 0), (0xc0, b'@', b'`', 0), (0xdb, b'[', b'{', 0x1b),
                (0xdc, b'\\', b'|', 0x1c), (0xdd, b']', b'}', 0x1d), (0xde, b'^', b'~', 0), (0xe2, b'\\', b'_', 0x1c),
            ];
            let t = if l == Layout::Us { &US } else { &JA };
            t.iter().find(|e| e.0 == vk).map_or((0, 0, 0), |e| (e.1 as u32, e.2 as u32, e.3 as u32))
        }
    }
}

/// With Ctrl and Shift: kbdus' and kbd106's control characters on shifted keys (Ctrl+Shift+6 is
/// RS, Ctrl+Shift+- US); else what Ctrl alone gives.
fn ctrl_shift(l: Layout, vk: u32) -> u32 {
    match (l, vk) {
        (_, 0x41..=0x5a) => vk - 0x40,
        (Layout::Us, 0x36) | (Layout::Ja, 0xde) => 0x1e,
        (Layout::Us, 0xbd) | (Layout::Ja, 0xe2) => 0x1f,
        _ => 0,
    }
}

/// The character a key gives with `state` (a GetKeyboardState array): Shift, Ctrl, Alt, CapsLock;
/// None for none (and for Ctrl+Alt: neither layout has AltGr characters).
pub fn translate(l: Layout, vk: u32, state: &[u8; 256]) -> Option<u32> {
    let down = |k: usize| state[k] & 0x80 != 0;
    let shift = down(0x10) || down(0xa0) || down(0xa1);
    let ctrl = down(0x11) || down(0xa2) || down(0xa3);
    let alt = down(0x12) || down(0xa4) || down(0xa5);
    if ctrl && alt {
        return None;
    }
    let (n, s, c) = chars(l, vk & 0xff);
    let ch = if ctrl {
        if shift { ctrl_shift(l, vk & 0xff) } else { c }
    } else {
        let caps = state[0x14] & 1 != 0 && (0x41..=0x5a).contains(&(vk & 0xff));
        if shift != caps { s } else { n }
    };
    (ch != 0).then_some(ch)
}

/// VkKeyScan: the key and shift state (1 Shift, 2 Ctrl) that type `ch`, as the layout's tables
/// are searched (the main keys before the keypad); None when no key types it.
pub fn scan_char(l: Layout, ch: u32) -> Option<u32> {
    // (the main keys: Backspace, Tab, Esc, Enter, Space, Break first, so that their own characters
    // are theirs and not Ctrl+H / Ctrl+[ / Ctrl+M's; then letters, digits, the rest)
    let keys = [0x08, 0x09, 0x1b, 0x0d, 0x20, 0x03].into_iter().chain(0x41..=0x5a).chain(0x30..=0x39).chain([0xba, 0xbb, 0xbc, 0xbd, 0xbe, 0xbf, 0xc0, 0xdb, 0xdc, 0xdd, 0xde, 0xe2]);
    for vk in keys {
        let (n, s, c) = chars(l, vk);
        for (v, st) in [(n, 0), (s, 1), (c, 2), (ctrl_shift(l, vk), 3)] {
            if v != 0 && v == ch && !(st == 3 && (0x41..=0x5a).contains(&vk)) {
                return Some(vk | st << 8);
            }
        }
    }
    None
}

/// MapVirtualKey(code, type) in `l`: 0 VK_TO_VSC, 1 VSC_TO_VK, 2 VK_TO_CHAR, 3 VSC_TO_VK_EX,
/// 4 VK_TO_VSC_EX.
pub fn map_vk(l: Layout, code: u32, ty: u32) -> u32 {
    match ty {
        // (PrintScreen: SysRq's scan code, E0 37 as an extended one)
        4 if code == 0x2c => 0xe037,
        0 | 4 => {
            let (s, ext) = vk_scan(l, code);
            if ty == 4 && ext && s != 0 { 0xe000 | s } else { s }
        }
        1 | 3 => {
            let (pre, s) = (code >> 8, code & 0xff);
            let vk = match pre {
                0 => scan_vk(l, s),
                0xe0 if ty == 3 => ext_vk(s),
                _ => 0,
            };
            if ty == 1 {
                match vk {
                    0xa0 | 0xa1 => SHIFT,
                    0xa2 | 0xa3 => CONTROL,
                    0xa4 | 0xa5 => MENU,
                    v => v,
                }
            } else {
                vk
            }
        }
        2 => {
            let (n, _, _) = chars(l, code);
            // (letters: their capital, as Windows answers)
            if (0x41..=0x5a).contains(&code) { code } else { n }
        }
        _ => 0,
    }
}

fn hkl_arg(c: &Ctx, i: u32) -> u32 {
    c.a(i)
}

pub fn MapVirtualKeyA(c: &mut Ctx) -> u32 {
    map_vk(layout_of(0), c.a(0), c.a(1))
}
pub fn MapVirtualKeyExA(c: &mut Ctx) -> u32 {
    map_vk(layout_of(hkl_arg(c, 2)), c.a(0), c.a(1))
}

/// ToUnicode(Ex)(vk, scan, state, buf, size, flags[, hkl]): the character, and a terminating
/// 0 when there is room; 1 or 0 (no character).
fn to_unicode(c: &mut Ctx, hkl: u32) -> u32 {
    let (vk, state, buf, size) = (c.a(0), c.a(2), c.a(3), c.a(4) as i32);
    let mut st = [0u8; 256];
    if state != 0 {
        st.copy_from_slice(mem::slice(state, 256));
    }
    let Some(ch) = translate(layout_of(hkl), vk, &st) else { return 0 };
    if buf == 0 || size <= 0 {
        return 0;
    }
    mem::w16(buf, ch as u16);
    if size > 1 {
        mem::w16(buf + 2, 0);
    }
    1
}
pub fn ToUnicode(c: &mut Ctx) -> u32 {
    to_unicode(c, 0)
}
pub fn ToUnicodeEx(c: &mut Ctx) -> u32 {
    let hkl = hkl_arg(c, 6);
    to_unicode(c, hkl)
}
/// ToAscii(Ex)(vk, scan, state, out WORD, flags[, hkl]): the character in the low byte.
fn to_ascii(c: &mut Ctx, hkl: u32) -> u32 {
    let (vk, state, out) = (c.a(0), c.a(2), c.a(3));
    let mut st = [0u8; 256];
    if state != 0 {
        st.copy_from_slice(mem::slice(state, 256));
    }
    let Some(ch) = translate(layout_of(hkl), vk, &st) else { return 0 };
    if out != 0 {
        mem::w16(out, ch as u16);
    }
    1
}
pub fn ToAscii(c: &mut Ctx) -> u32 {
    to_ascii(c, 0)
}
pub fn ToAsciiEx(c: &mut Ctx) -> u32 {
    let hkl = hkl_arg(c, 5);
    to_ascii(c, hkl)
}

/// VkKeyScan(Ex): -1 (a SHORT) when no key types the character.
fn vk_key_scan(ch: u32, hkl: u32) -> u32 {
    scan_char(layout_of(hkl), ch).unwrap_or(0xffff_ffff)
}
pub fn VkKeyScanA(c: &mut Ctx) -> u32 {
    vk_key_scan(c.a(0) & 0xff, 0)
}
pub fn VkKeyScanW(c: &mut Ctx) -> u32 {
    vk_key_scan(c.a(0) & 0xffff, 0)
}
pub fn VkKeyScanExA(c: &mut Ctx) -> u32 {
    let hkl = hkl_arg(c, 1);
    vk_key_scan(c.a(0) & 0xff, hkl)
}
pub fn VkKeyScanExW(c: &mut Ctx) -> u32 {
    let hkl = hkl_arg(c, 1);
    vk_key_scan(c.a(0) & 0xffff, hkl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn state(keys: &[usize]) -> [u8; 256] {
        let mut s = [0u8; 256];
        for &k in keys {
            s[k] = 0x80;
        }
        s
    }

    #[test]
    fn what_windows_answers() {
        for l in [Layout::Us, Layout::Ja] {
            // (Wine's user32 input tests: shifts are one scan code each side, the generic VK
            // for VSC_TO_VK, the sided one for VSC_TO_VK_EX)
            assert_eq!(map_vk(l, 0x10, 0), 0x2a);
            assert_eq!(map_vk(l, 0xa0, 0), 0x2a);
            assert_eq!(map_vk(l, 0xa1, 0), 0x36);
            assert_eq!(map_vk(l, 0x2a, 1), 0x10);
            assert_eq!(map_vk(l, 0x36, 1), 0x10);
            assert_eq!(map_vk(l, 0x2a, 3), 0xa0);
            assert_eq!(map_vk(l, 0x36, 3), 0xa1);
            assert_eq!(map_vk(l, 0xa3, 0), 0x1d);
            assert_eq!(map_vk(l, 0xa3, 4), 0xe01d);
            assert_eq!(map_vk(l, 0xe01d, 3), 0xa3);
            assert_eq!(map_vk(l, 0x1d, 3), 0xa2);
            assert_eq!(map_vk(l, 0xe038, 3), 0xa5);
            assert_eq!(map_vk(l, 0x24, 0), 0x47);
            assert_eq!(map_vk(l, 0x24, 4), 0xe047);
            assert_eq!(map_vk(l, 0x47, 1), 0x24);
            assert_eq!(map_vk(l, 0x67, 0), 0x47, "VK_NUMPAD7 is the Home key");
            assert_eq!(map_vk(l, 0x41, 0), 0x1e);
            assert_eq!(map_vk(l, 0x1e, 1), 0x41);
            assert_eq!(map_vk(l, 0x41, 2), 0x41, "letters: capital");
            assert_eq!(map_vk(l, 0x31, 2), 0x31);
            assert_eq!(map_vk(l, 0x0d, 2), 0x0d);
            assert_eq!(map_vk(l, 0x70, 0), 0x3b);
            assert_eq!(map_vk(l, 0x7b, 0), 0x58, "F12");
            assert_eq!(map_vk(l, 0x41, 9), 0);
            assert_eq!(translate(l, 0x41, &state(&[])), Some('a' as u32));
            assert_eq!(translate(l, 0x41, &state(&[0x10])), Some('A' as u32));
            let mut caps = state(&[]);
            caps[0x14] = 1;
            assert_eq!(translate(l, 0x41, &caps), Some('A' as u32));
            assert_eq!(translate(l, 0x31, &caps), Some('1' as u32), "CapsLock: letters only");
            assert_eq!(translate(l, 0x41, &state(&[0x11])), Some(1));
            assert_eq!(translate(l, 0x41, &state(&[0x11, 0x12])), None);
            assert_eq!(translate(l, 0x0d, &state(&[0x11])), Some(0x0a));
            assert_eq!(translate(l, 0x10, &state(&[0x10])), None);
            assert_eq!(scan_char(l, 'a' as u32), Some(0x41));
            assert_eq!(scan_char(l, 'A' as u32), Some(0x141));
            assert_eq!(scan_char(l, 1), Some(0x241));
            assert_eq!(scan_char(l, 0x0d), Some(0x0d));
            assert_eq!(scan_char(l, 0x0a), Some(0x20d));
            assert_eq!(scan_char(l, 0x1b), Some(0x1b));
            assert_eq!(scan_char(l, 8), Some(8));
            assert_eq!(scan_char(l, 9), Some(9));
            assert_eq!(scan_char(l, 0x3042), None);
        }
        assert_eq!(translate(Layout::Us, 0x32, &state(&[0x10])), Some('@' as u32));
        assert_eq!(translate(Layout::Ja, 0x32, &state(&[0x10])), Some('"' as u32));
        assert_eq!(translate(Layout::Ja, 0xc0, &state(&[])), Some('@' as u32));
        assert_eq!(scan_char(Layout::Us, '@' as u32), Some(0x132));
        assert_eq!(scan_char(Layout::Ja, '@' as u32), Some(0xc0));
        assert_eq!(scan_char(Layout::Us, '*' as u32), Some(0x138), "the main keys before the keypad");
        assert_eq!(map_vk(Layout::Us, 0xde, 0), 0x28);
        assert_eq!(map_vk(Layout::Ja, 0xde, 0), 0x0d);
        assert_eq!(map_vk(Layout::Ja, 0x7d, 1), 0xdc, "¥");
    }

    proptest! {
        /// Scan code -> key -> scan code comes back (the keys made by one scan code each), and
        /// the _EX forms agree with the plain ones.
        #[test]
        fn scan_codes_round_trip(ja in any::<bool>(), s in 1u32..0x80, ext in any::<bool>()) {
            let l = if ja { Layout::Ja } else { Layout::Us };
            let code = if ext { 0xe000 | s } else { s };
            let vk = map_vk(l, code, 3);
            // (but SysRq / PrintScreen: 54 one way, E0 37 the other)
            if vk != 0 && code != 0x54 {
                let back = map_vk(l, vk, 4);
                // (keypad keys share their scan codes with the navigation keys: E0 or not)
                prop_assert_eq!(back & 0xff, s, "{:#x} -> {:#x} -> {:#x}", code, vk, back);
                if !ext {
                    let g = map_vk(l, code, 1);
                    prop_assert!(g == vk || matches!((vk, g), (0xa0 | 0xa1, 0x10) | (0xa2, 0x11) | (0xa4, 0x12)));
                    prop_assert_eq!(map_vk(l, vk, 0), s);
                }
            }
        }

        /// Whatever VkKeyScan says types a character types it: ToUnicode with that shift state
        /// gives the character back.
        #[test]
        fn vkkeyscan_and_tounicode_agree(ja in any::<bool>(), ch in 1u32..0x80) {
            let l = if ja { Layout::Ja } else { Layout::Us };
            if let Some(k) = scan_char(l, ch) {
                let mut st = [0u8; 256];
                if k & 0x100 != 0 { st[0x10] = 0x80; }
                if k & 0x200 != 0 { st[0x11] = 0x80; }
                prop_assert_eq!(translate(l, k & 0xff, &st), Some(ch), "{:#x} -> {:#x}", ch, k);
            } else {
                // (printable ASCII: on both keyboards)
                prop_assert!(!(0x20..0x7f).contains(&ch), "{:#x}", ch);
            }
        }

        /// Any key in any state gives at most one character, never one Ctrl+Alt makes.
        #[test]
        fn any_state(ja in any::<bool>(), vk in 0u32..256, bits in proptest::collection::vec((0usize..256, any::<u8>()), 0..8)) {
            let l = if ja { Layout::Ja } else { Layout::Us };
            let mut st = [0u8; 256];
            for (k, v) in bits { st[k] = v; }
            let r = translate(l, vk, &st);
            let ctrl = st[0x11] & 0x80 != 0 || st[0xa2] & 0x80 != 0 || st[0xa3] & 0x80 != 0;
            let alt = st[0x12] & 0x80 != 0 || st[0xa4] & 0x80 != 0 || st[0xa5] & 0x80 != 0;
            if ctrl && alt { prop_assert_eq!(r, None); }
            if let Some(c) = r { prop_assert!(c < 0x80); }
        }
    }
}
