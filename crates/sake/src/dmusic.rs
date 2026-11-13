//! DirectMusic (dmusic.dll's CLSID_DirectMusic): the object without ports. Programs built on the
//! DirectX SDK's GetDXVersion() create it to tell DirectX 6.1 from 6.0, and refuse to start below
//! their minimum (SinsAbell 体験版: "DirectX 8.0 以降のバージョンのインストールが必要です ...
//! 現在のバージョン : DirectX 6" when the class was not registered). There is no synthesizer:
//! EnumPort and EnumMasterClock list nothing, and CreatePort, CreateMusicBuffer, GetMasterClock
//! and GetDefaultPort fail, so a game that plays MIDI through it sees no device and no music.

use crate::api::{Ctx, Handler};
use crate::mem;

const S_FALSE: u32 = 1;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_POINTER: u32 = 0x8000_4003;
/// dmerror.h DMUS_E_NOT_FOUND ("the requested item was not found"): CreatePort and
/// GetDefaultPort with no port in the system
const DMUS_E_NOT_FOUND: u32 = 0x8878_1161;

/// CLSID_DirectMusic {636B9F10-0C7D-11D1-95B2-0020AFDC7421}
pub const CLSID_DIRECTMUSIC: [u8; 16] = guid(0x636b_9f10, 0x0c7d, 0x11d1, [0x95, 0xb2, 0x00, 0x20, 0xaf, 0xdc, 0x74, 0x21]);
/// IUnknown, IID_IDirectMusic, IID_IDirectMusic2 (the same vtable as IDirectMusic), IID_IDirectMusic8
const IIDS: [[u8; 16]; 4] = [
    guid(0, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]),
    guid(0x6536_115a, 0x7b2d, 0x11d2, [0xba, 0x18, 0x00, 0x00, 0xf8, 0x75, 0xac, 0x12]),
    guid(0x6fc2_cae1, 0xbc78, 0x11d2, [0xaf, 0xa6, 0x00, 0xaa, 0x00, 0x24, 0xd8, 0xb6]),
    guid(0x2d36_29f7, 0x813d, 0x4939, [0x85, 0x08, 0xf0, 0x5c, 0x6b, 0x75, 0xfd, 0x97]),
];

const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let a = a.to_le_bytes();
    let (b, c) = (b.to_le_bytes(), c.to_le_bytes());
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]
}

/// Whether the object answers QueryInterface for `iid` (every interface is the one object:
/// IDirectMusic8 extends IDirectMusic(2) by one method at the end).
pub fn answers(iid: &[u8; 16]) -> bool {
    IIDS.contains(iid)
}

/// The reference count after Release of an object holding `refs`, and whether it is freed.
/// (Release of a freed object is the program's bug: the count stays 0.)
pub fn released(refs: u32) -> (u32, bool) {
    match refs {
        0 => (0, false),
        1 => (0, true),
        n => (n - 1, false),
    }
}

fn read16(p: u32) -> [u8; 16] {
    let mut g = [0u8; 16];
    g.copy_from_slice(mem::slice(p, 16));
    g
}

/// CoCreateInstance(CLSID_DirectMusic, outer, context, iid, &out): None for another class.
pub fn create_instance(clsid: u32, outer: u32, iid: u32, out: u32) -> Option<u32> {
    if read16(clsid) != CLSID_DIRECTMUSIC {
        return None;
    }
    if out == 0 {
        return Some(E_POINTER);
    }
    mem::putp(out, 0);
    // (dmusic does not aggregate)
    if outer != 0 {
        return Some(0x8004_0110); // CLASS_E_NOAGGREGATION
    }
    if iid == 0 || !answers(&read16(iid)) {
        return Some(E_NOINTERFACE);
    }
    mem::putp(out, new_object());
    Some(0)
}

/// Layout: vtable, references.
fn new_object() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *V.get_or_init(|| {
        // (stack bytes, `this` included)
        let m: &[(Handler, u32)] = &[
            (qi, 12),
            (add_ref, 4),
            (release, 4),
            (enum_none, 12),     // EnumPort(index, DMUS_PORTCAPS*)
            (not_impl, 16),      // CreateMusicBuffer(desc, &buffer, outer)
            (create_port, 20),   // CreatePort(clsid, params, &port, outer)
            (enum_none, 12),     // EnumMasterClock(index, DMUS_CLOCKINFO*)
            (get_clock, 12),     // GetMasterClock(guid*, &clock)
            (ok, 8),             // SetMasterClock(guid)
            (ok, 8),             // Activate(enable)
            (not_found, 8),      // GetDefaultPort(guid*)
            (ok, 12),            // SetDirectSound(dsound, hwnd)
            (not_impl, 8),       // IDirectMusic8::SetExternalMasterClock(clock)
        ];
        crate::api::vtable("IDirectMusic8", m)
    });
    let o = mem::heap_alloc(2 * mem::ps(), true);
    mem::wp(o, vt);
    mem::w32(o + mem::ps(), 1);
    o
}

fn qi(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if iid == 0 || !answers(&read16(iid)) {
        mem::putp(out, 0);
        return E_NOINTERFACE;
    }
    add_ref(c);
    mem::putp(out, this);
    0
}
fn add_ref(c: &mut Ctx) -> u32 {
    let r = c.a(0) + mem::ps();
    let n = mem::r32(r).saturating_add(1);
    mem::w32(r, n);
    n
}
fn release(c: &mut Ctx) -> u32 {
    let r = c.a(0) + mem::ps();
    let (n, _freed) = released(mem::r32(r));
    // (the memory stays: a stale pointer still finds a vtable)
    mem::w32(r, n);
    n
}
fn enum_none(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 { E_POINTER } else { S_FALSE }
}
fn create_port(c: &mut Ctx) -> u32 {
    if c.a(3) != 0 {
        mem::putp(c.a(3), 0);
    }
    DMUS_E_NOT_FOUND
}
fn get_clock(c: &mut Ctx) -> u32 {
    if c.a(2) != 0 {
        mem::putp(c.a(2), 0);
    }
    E_NOTIMPL
}
fn not_found(_: &mut Ctx) -> u32 {
    DMUS_E_NOT_FOUND
}
fn not_impl(_: &mut Ctx) -> u32 {
    E_NOTIMPL
}
fn ok(_: &mut Ctx) -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The GUIDs as dmusici.h / dmusicc.h spell them, byte for byte (little-endian fields).
    #[test]
    fn guids() {
        assert_eq!(CLSID_DIRECTMUSIC, [0x10, 0x9f, 0x6b, 0x63, 0x7d, 0x0c, 0xd1, 0x11, 0x95, 0xb2, 0x00, 0x20, 0xaf, 0xdc, 0x74, 0x21]);
        assert_eq!(IIDS[1], [0x5a, 0x11, 0x36, 0x65, 0x2d, 0x7b, 0xd2, 0x11, 0xba, 0x18, 0x00, 0x00, 0xf8, 0x75, 0xac, 0x12]);
        assert!(answers(&IIDS[3]));
        // not the class itself, not IClassFactory, not IDirectSound
        assert!(!answers(&CLSID_DIRECTMUSIC));
        assert!(!answers(&guid(1, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46])));
        assert!(!answers(&guid(0x279a_fa83, 0x4981, 0x11ce, [0xa5, 0x21, 0x00, 0x20, 0xaf, 0x0b, 0xe5, 0x60])));
    }

    #[test]
    fn release_counts() {
        assert_eq!(released(2), (1, false));
        assert_eq!(released(1), (0, true));
        assert_eq!(released(0), (0, false));
    }

    proptest! {
        /// Only the four interfaces: any other 16 bytes, and any one-bit change of an IID, are refused.
        #[test]
        fn qi_only_known(g in any::<[u8; 16]>(), i in 0usize..4, bit in 0usize..128) {
            prop_assert_eq!(answers(&g), IIDS.contains(&g));
            let mut f = IIDS[i];
            f[bit / 8] ^= 1 << (bit % 8);
            prop_assert!(!answers(&f));
        }

        /// AddRef n times then Release n + 1 times frees the object exactly once, at the last.
        #[test]
        fn refcount_frees_once(n in 0u32..1000) {
            let mut refs = 1 + n;
            let mut frees = 0;
            for k in 0..=n {
                let (r, freed) = released(refs);
                refs = r;
                frees += freed as u32;
                prop_assert_eq!(freed, k == n);
            }
            prop_assert_eq!((refs, frees), (0, 1));
            prop_assert_eq!(released(refs), (0, false));
        }
    }
}
