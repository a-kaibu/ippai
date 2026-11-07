//! mmdevapi: the multimedia device enumerator (CLSID_MMDeviceEnumerator) with one render
//! endpoint, the browser's audio output, and no capture endpoints. Games ask it for the default
//! device and register for device-change notifications (AdvHD dereferences the enumerator
//! without checking CoCreateInstance); the device's IAudioClient is wasapi.rs.
//! The enumerator is a process-wide singleton holding a reference of its own (CoCreateInstance
//! gives it a second one, as Windows' does); collections are separate objects freed on their
//! last Release; the device, its endpoint and property store are stateless singletons.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::api::{Ctx, Handler};
use crate::mem::{self, ps, putp, r16, r32, w16, w32, wp};

const S_OK: u32 = 0;
const E_POINTER: u32 = 0x8000_4003;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_INVALIDARG: u32 = 0x8007_0057;
/// HRESULT_FROM_WIN32(ERROR_NOT_FOUND)
const E_NOTFOUND: u32 = 0x8007_0490;
const E_RENDER: u32 = 0;
const E_ALL: u32 = 2;
const DEVICE_STATE_ACTIVE: u32 = 1;
/// The endpoint's id (as Windows forms them: flow.{endpoint guid})
pub(crate) const DEVICE_ID: &str = "{0.0.0.00000000}.{5a2c1e0b-7f3d-4e8a-9c61-3b8d0f4a2e17}";
const FRIENDLY_NAME: &str = "スピーカー (sake Audio)";

/// x86 stdcall: the handler pops this many bytes of arguments (x64 ignores it)
fn b(n: u32) -> u32 {
    4 * n
}

fn singleton(name: &str, methods: &[(Handler, u32)], cell: &OnceLock<u32>) -> u32 {
    *cell.get_or_init(|| {
        let o = mem::heap_alloc(2 * ps(), true);
        wp(o, crate::api::vtable(name, methods));
        o
    })
}
fn guid_is(p: u32, d1: u32, d2: u16, d3: u16) -> bool {
    p != 0 && r32(p) == d1 && r16(p + 4) == d2 && r16(p + 6) == d3
}
fn is_unknown(iid: u32) -> bool {
    iid != 0 && (0..4).all(|i| r32(iid + 4 * i) == [0, 0, 0xc0, 0x4600_0000][i as usize])
}
fn one(_: &mut Ctx) -> u32 {
    1
}
/// QueryInterface answering IUnknown and the object's own interface
fn qi(c: &mut Ctx, own: (u32, u16, u16)) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if is_unknown(iid) || guid_is(iid, own.0, own.1, own.2) {
        putp(out, this);
        S_OK
    } else {
        putp(out, 0);
        E_NOINTERFACE
    }
}
/// A CoTaskMemAlloc'd copy of `s` (UTF-16, NUL-terminated)
fn task_wstr(s: &str) -> u32 {
    let w: Vec<u16> = s.encode_utf16().chain([0]).collect();
    let p = mem::heap_alloc(2 * w.len() as u32, false);
    for (i, v) in w.iter().enumerate() {
        w16(p + 2 * i as u32, *v);
    }
    p
}

// ---------------------------------------------------------------------------------------------
// IMMDeviceEnumerator {a95664d2-9614-4f35-a746-de8db63617e6}

fn en_qi(c: &mut Ctx) -> u32 {
    qi(c, (0xa956_64d2, 0x9614, 0x4f35))
}
/// EnumAudioEndpoints(flow, state mask, IMMDeviceCollection**)
fn en_enum(c: &mut Ctx) -> u32 {
    let (flow, mask, out) = (c.a(1), c.a(2), c.a(3));
    if out == 0 {
        return E_POINTER;
    }
    if flow > E_ALL || mask & !0xf != 0 {
        putp(out, 0);
        return E_INVALIDARG;
    }
    let has = (flow == E_RENDER || flow == E_ALL) && mask & DEVICE_STATE_ACTIVE != 0;
    putp(out, collection(has));
    S_OK
}
/// GetDefaultAudioEndpoint(flow, role, IMMDevice**): the render endpoint; no capture one
fn en_default(c: &mut Ctx) -> u32 {
    let (flow, role, out) = (c.a(1), c.a(2), c.a(3));
    if out == 0 {
        return E_POINTER;
    }
    putp(out, 0);
    if flow > E_ALL || role > 2 {
        return E_INVALIDARG;
    }
    if flow != E_RENDER {
        return E_NOTFOUND;
    }
    putp(out, device());
    S_OK
}
/// GetDevice(id, IMMDevice**): an id that names no endpoint is E_INVALIDARG (Windows, Wine's
/// mmdevenum test)
fn en_get(c: &mut Ctx) -> u32 {
    let (id, out) = (c.a(1), c.a(2));
    if id == 0 || out == 0 {
        return E_POINTER;
    }
    let want: Vec<u16> = DEVICE_ID.encode_utf16().collect();
    if mem::wstr(id) == want {
        putp(out, device());
        S_OK
    } else {
        putp(out, 0);
        E_INVALIDARG
    }
}
/// The registered IMMNotificationClient pointers (one entry per registration; the one device
/// never changes, so none is called; Windows takes no reference on them)
static CLIENTS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
/// RegisterEndpointNotificationCallback(IMMNotificationClient*)
fn en_register(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    CLIENTS.lock().unwrap_or_else(|e| e.into_inner()).push(c.a(1));
    S_OK
}
/// UnregisterEndpointNotificationCallback(IMMNotificationClient*): one registration of it
fn en_unregister(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let mut l = CLIENTS.lock().unwrap_or_else(|e| e.into_inner());
    match l.iter().position(|&p| p == c.a(1)) {
        Some(i) => {
            l.remove(i);
            S_OK
        }
        None => E_NOTFOUND,
    }
}
/// The enumerator's references: its own one plus its users'; it is never freed
static ENUM_REFS: AtomicU32 = AtomicU32::new(1);
fn en_addref(_: &mut Ctx) -> u32 {
    ENUM_REFS.fetch_add(1, Ordering::Relaxed) + 1
}
// (fetch_update is try_update on newer toolchains, as in mem.rs)
#[allow(deprecated)]
fn en_release(_: &mut Ctx) -> u32 {
    let r = ENUM_REFS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |r| Some(r.saturating_sub(1).max(1)));
    r.unwrap_or(1).saturating_sub(1).max(1)
}
fn en_qi_ref(c: &mut Ctx) -> u32 {
    let r = en_qi(c);
    if r == S_OK {
        ENUM_REFS.fetch_add(1, Ordering::Relaxed);
    }
    r
}
fn enumerator_object() -> u32 {
    static O: OnceLock<u32> = OnceLock::new();
    singleton("IMMDeviceEnumerator", &[(en_qi_ref, b(3)), (en_addref, b(1)), (en_release, b(1)), (en_enum, b(4)), (en_default, b(4)), (en_get, b(3)), (en_register, b(2)), (en_unregister, b(2))], &O)
}
/// The enumerator with a new reference for the caller
pub fn enumerator() -> u32 {
    let o = enumerator_object();
    ENUM_REFS.fetch_add(1, Ordering::Relaxed);
    o
}

// ---------------------------------------------------------------------------------------------
// IMMDeviceCollection {0bd7a1be-7a1a-44db-8397-cc5392387b5e}: none or the one device; the
// object is [vtable, has, references] and takes no reference on the enumerator

fn col_qi(c: &mut Ctx) -> u32 {
    let r = qi(c, (0x0bd7_a1be, 0x7a1a, 0x44db));
    if r == S_OK {
        col_addref(c);
    }
    r
}
fn col_addref(c: &mut Ctx) -> u32 {
    let p = c.a(0) + 2 * ps();
    w32(p, r32(p) + 1);
    r32(p)
}
fn col_release(c: &mut Ctx) -> u32 {
    let p = c.a(0) + 2 * ps();
    let r = r32(p).saturating_sub(1);
    w32(p, r);
    if r == 0 {
        mem::heap_free(c.a(0));
    }
    r
}
fn col_has(this: u32) -> bool {
    r32(this + ps()) != 0
}
fn col_count(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), col_has(c.a(0)) as u32);
    S_OK
}
fn col_item(c: &mut Ctx) -> u32 {
    let (this, i, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if i == 0 && col_has(this) {
        putp(out, device());
        S_OK
    } else {
        putp(out, 0);
        E_INVALIDARG
    }
}
fn collection(has: bool) -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    let v = *V.get_or_init(|| crate::api::vtable("IMMDeviceCollection", &[(col_qi, b(3)), (col_addref, b(1)), (col_release, b(1)), (col_count, b(2)), (col_item, b(3))]));
    let o = mem::heap_alloc(3 * ps(), true);
    wp(o, v);
    w32(o + ps(), has as u32);
    w32(o + 2 * ps(), 1);
    o
}

// ---------------------------------------------------------------------------------------------
// IMMDevice {d666063f-1587-4e43-81f1-b948e807363f}

/// IMMDevice and, as Windows' device object has, IMMEndpoint (Unity 6's audio setup asks for it
/// and uses the pointer without checking the result)
fn dev_qi(c: &mut Ctx) -> u32 {
    let (iid, out) = (c.a(1), c.a(2));
    if out != 0 && guid_is(iid, ENDPOINT.0, ENDPOINT.1, ENDPOINT.2) {
        putp(out, endpoint());
        return S_OK;
    }
    qi(c, (0xd666_063f, 0x1587, 0x4e43))
}
/// Activate(iid, clsctx, params, void**): WASAPI's IAudioClient, IAudioSessionManager(2) and
/// IAudioEndpointVolume (wasapi.rs), or the property store
fn dev_activate(c: &mut Ctx) -> u32 {
    let (iid, out) = (c.a(1), c.a(4));
    if iid == 0 || out == 0 {
        return E_POINTER;
    }
    if guid_is(iid, 0x886d_8eeb, 0x8cf2, 0x4446) {
        putp(out, store());
        return S_OK;
    }
    // IDirectSound(8) {279afa83-4981-11ce-a521-0020af0be560}, {c50a7e93-f395-4834-9ef6-7fa99de50966}:
    // a DirectSound device on this endpoint, dsound.dll loaded as Windows' does
    if guid_is(iid, 0x279a_fa83, 0x4981, 0x11ce) || guid_is(iid, 0xc50a_7e93, 0xf395, 0x4834) {
        crate::pe::load_library(c.cpu, "dsound.dll");
        if let Some(o) = crate::sound::device() {
            putp(out, o);
            return S_OK;
        }
    }
    // WASAPI (wasapi.rs): a stream, the session manager, the endpoint's volume
    use crate::wasapi::{IID_CLIENT, IID_ENDPOINT_VOLUME, IID_MANAGER, IID_MANAGER2};
    let is = |g: (u32, u16, u16)| guid_is(iid, g.0, g.1, g.2);
    let o = if is(IID_CLIENT) {
        crate::wasapi::new_client()
    } else if is(IID_MANAGER) || is(IID_MANAGER2) {
        crate::wasapi::manager()
    } else if is(IID_ENDPOINT_VOLUME) {
        crate::wasapi::endpoint_volume()
    } else {
        0
    };
    if o != 0 {
        putp(out, o);
        return S_OK;
    }
    crate::com::warn_once(&format!("IMMDevice::Activate {:08x}: not available", r32(iid)));
    putp(out, 0);
    E_NOINTERFACE
}
/// OpenPropertyStore(access, IPropertyStore**): STGM_READ, _WRITE or _READWRITE; another
/// access is E_INVALIDARG with the pointer left as it was (Wine's propstore test)
fn dev_store(c: &mut Ctx) -> u32 {
    if c.a(1) > 2 {
        return E_INVALIDARG;
    }
    if c.a(2) == 0 {
        return E_POINTER;
    }
    putp(c.a(2), store());
    S_OK
}
/// GetId(LPWSTR*)
fn dev_id(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    putp(c.a(1), task_wstr(DEVICE_ID));
    S_OK
}
fn dev_state(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), DEVICE_STATE_ACTIVE);
    S_OK
}
fn device() -> u32 {
    static O: OnceLock<u32> = OnceLock::new();
    singleton("IMMDevice", &[(dev_qi, b(3)), (one, b(1)), (one, b(1)), (dev_activate, b(5)), (dev_store, b(3)), (dev_id, b(2)), (dev_state, b(2))], &O)
}

// ---------------------------------------------------------------------------------------------
// IMMEndpoint {1be09788-6894-4089-8586-9a2a6c265ac5}: the device's second interface (IUnknown
// and IMMDevice through it are the device, as COM's identity rule wants)

const ENDPOINT: (u32, u16, u16) = (0x1be0_9788, 0x6894, 0x4089);
fn ep_qi(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let to = if guid_is(iid, ENDPOINT.0, ENDPOINT.1, ENDPOINT.2) {
        this
    } else if is_unknown(iid) || guid_is(iid, 0xd666_063f, 0x1587, 0x4e43) {
        device()
    } else {
        putp(out, 0);
        return E_NOINTERFACE;
    };
    putp(out, to);
    S_OK
}
/// GetDataFlow(EDataFlow*): the one endpoint renders
fn ep_flow(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), E_RENDER);
    S_OK
}
fn endpoint() -> u32 {
    static O: OnceLock<u32> = OnceLock::new();
    singleton("IMMEndpoint", &[(ep_qi, b(3)), (one, b(1)), (one, b(1)), (ep_flow, b(2))], &O)
}

// ---------------------------------------------------------------------------------------------
// IPropertyStore {886d8eeb-8cf2-4446-8d02-cdba1dbdcf99}: the names, read-only

/// A property's value
enum Val {
    Str(&'static str),
    /// VT_UI4
    U4(u32),
    /// VT_BLOB holding the device format (WAVEFORMATEXTENSIBLE)
    DeviceFormat,
}
/// The keys answered (fmtid, pid, value): PKEY_Device_FriendlyName, PKEY_Device_DeviceDesc,
/// PKEY_DeviceInterface_FriendlyName, PKEY_AudioEndpoint_FormFactor (Speakers),
/// PKEY_AudioEndpoint_GUID, PKEY_AudioEngine_DeviceFormat and the device interface path
/// {233164c8-1b2c-4c7d-bc68-b671687a2567},1
const KEYS: [([u8; 16], u32, Val); 7] = [
    (guid(0xa45c_254e, 0xdf1c, 0x4efd, [0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0]), 14, Val::Str(FRIENDLY_NAME)),
    (guid(0xa45c_254e, 0xdf1c, 0x4efd, [0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0]), 2, Val::Str("スピーカー")),
    (guid(0x026e_516e, 0xb814, 0x414b, [0x83, 0xcd, 0x85, 0x6d, 0x6f, 0xef, 0x48, 0x22]), 2, Val::Str("sake Audio")),
    (guid(0x1da5_d803, 0xd492, 0x4edd, [0x8c, 0x23, 0xe0, 0xc0, 0xff, 0xee, 0x7f, 0x0e]), 0, Val::U4(1)),
    (guid(0x1da5_d803, 0xd492, 0x4edd, [0x8c, 0x23, 0xe0, 0xc0, 0xff, 0xee, 0x7f, 0x0e]), 4, Val::Str(ENDPOINT_GUID)),
    (guid(0xf19f_064d, 0x082c, 0x4e27, [0xbc, 0x73, 0x68, 0x82, 0xa1, 0xbb, 0x8e, 0x4c]), 0, Val::DeviceFormat),
    (guid(0x2331_64c8, 0x1b2c, 0x4c7d, [0xbc, 0x68, 0xb6, 0x71, 0x68, 0x7a, 0x25, 0x67]), 1, Val::Str(INTERFACE_PATH)),
];
/// The endpoint's guid, as DEVICE_ID ends
const ENDPOINT_GUID: &str = "{5a2c1e0b-7f3d-4e8a-9c61-3b8d0f4a2e17}";
const INTERFACE_PATH: &str = "\\\\?\\SWD#MMDEVAPI#{0.0.0.00000000}.{5a2c1e0b-7f3d-4e8a-9c61-3b8d0f4a2e17}#{e6327cad-dcec-4949-ae8a-991e976a79d2}";
const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> [u8; 16] {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]]
}
/// The device format Windows reports for a typical stereo output: 16-bit PCM, 44.1 kHz
/// (the engine mixes in float, wasapi.rs's mix format)
fn device_format() -> [u8; 40] {
    let mut f = [0u8; 40];
    let put = |f: &mut [u8; 40], o: usize, v: &[u8]| f[o..o + v.len()].copy_from_slice(v);
    put(&mut f, 0, &0xfffeu16.to_le_bytes());
    put(&mut f, 2, &2u16.to_le_bytes());
    put(&mut f, 4, &44100u32.to_le_bytes());
    put(&mut f, 8, &(44100u32 * 4).to_le_bytes());
    put(&mut f, 12, &4u16.to_le_bytes());
    put(&mut f, 14, &16u16.to_le_bytes());
    put(&mut f, 16, &22u16.to_le_bytes());
    put(&mut f, 18, &16u16.to_le_bytes());
    put(&mut f, 20, &3u32.to_le_bytes());
    // KSDATAFORMAT_SUBTYPE_PCM {00000001-0000-0010-8000-00aa00389b71}
    put(&mut f, 24, &guid(1, 0, 0x10, [0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71]));
    f
}
fn ps_qi(c: &mut Ctx) -> u32 {
    qi(c, (0x886d_8eeb, 0x8cf2, 0x4446))
}
fn ps_count(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), KEYS.len() as u32);
    S_OK
}
/// GetAt(i, PROPERTYKEY*)
fn ps_at(c: &mut Ctx) -> u32 {
    let (i, key) = (c.a(1) as usize, c.a(2));
    if key == 0 {
        return E_POINTER;
    }
    let Some((g, pid, _)) = KEYS.get(i) else { return E_INVALIDARG };
    mem::write(key, g);
    w32(key + 16, *pid);
    S_OK
}
/// GetValue(REFPROPERTYKEY, PROPVARIANT*): the value, VT_EMPTY for another key
fn ps_get(c: &mut Ctx) -> u32 {
    let (key, v) = (c.a(1), c.a(2));
    if key == 0 || v == 0 {
        return E_POINTER;
    }
    mem::fill(v, 8 + 2 * ps(), 0);
    let k = mem::slice(key, 16);
    let pid = r32(key + 16);
    match KEYS.iter().find(|(g, p, _)| g[..] == *k && *p == pid).map(|e| &e.2) {
        Some(Val::Str(s)) => {
            w16(v, 31);
            putp(v + 8, task_wstr(s));
        }
        Some(Val::U4(n)) => {
            w16(v, 19);
            w32(v + 8, *n);
        }
        Some(Val::DeviceFormat) => {
            let f = device_format();
            let p = mem::heap_alloc(f.len() as u32, false);
            mem::write(p, &f);
            w16(v, 65);
            w32(v + 8, f.len() as u32);
            putp(v + 8 + ps(), p);
        }
        None => {}
    }
    S_OK
}
/// SetValue / Commit: STGM_READ stores
fn ps_denied(_: &mut Ctx) -> u32 {
    0x8003_0005
}
fn store() -> u32 {
    static O: OnceLock<u32> = OnceLock::new();
    singleton("IPropertyStore", &[(ps_qi, b(3)), (one, b(1)), (one, b(1)), (ps_count, b(2)), (ps_at, b(3)), (ps_get, b(3)), (ps_denied, b(3)), (ps_denied, b(1))], &O)
}

/// CoCreateInstance of CLSID_MMDeviceEnumerator {bcde0395-e52f-467c-8e3d-c4579291692e}
pub fn create_instance(clsid: u32) -> Option<u32> {
    guid_is(clsid, 0xbcde_0395, 0xe52f, 0x467c).then(enumerator)
}


#[cfg(test)]
mod tests {
    use super::*;

    /// PROPERTYKEYs are laid out as Windows' headers have them (fmtid's first three fields
    /// little-endian), and each key is answered once
    #[test]
    fn property_keys_are_windows_bytes_and_unique() {
        // PKEY_Device_FriendlyName {a45c254e-df1c-4efd-8020-67d146a850e0},14 as stored in memory
        assert_eq!(KEYS[0].0, [0x4e, 0x25, 0x5c, 0xa4, 0x1c, 0xdf, 0xfd, 0x4e, 0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0]);
        assert_eq!(KEYS[0].1, 14);
        for (i, a) in KEYS.iter().enumerate() {
            for b in &KEYS[i + 1..] {
                assert!(a.0 != b.0 || a.1 != b.1, "key {i} twice");
            }
        }
        // Device_FriendlyName is "DeviceDesc (DeviceInterface_FriendlyName)" (Wine's propstore test)
        let s = |i: usize| match KEYS[i].2 {
            Val::Str(s) => s,
            _ => panic!(),
        };
        assert_eq!(s(0), format!("{} ({})", s(1), s(2)));
        assert!(DEVICE_ID.ends_with(ENDPOINT_GUID) && INTERFACE_PATH.contains(DEVICE_ID));
    }

    /// The device format is a well-formed WAVEFORMATEXTENSIBLE that the engine plays in shared
    /// mode as it is (the same rate and channels as the mix format)
    #[test]
    fn device_format_is_a_shared_mode_format() {
        let f = device_format();
        let u16_ = |o: usize| u16::from_le_bytes([f[o], f[o + 1]]);
        let u32_ = |o: usize| u32::from_le_bytes([f[o], f[o + 1], f[o + 2], f[o + 3]]);
        let w = crate::wasapi::Wave {
            tag: u16_(0),
            channels: u16_(2),
            rate: u32_(4),
            avg: u32_(8),
            block: u16_(12),
            bits: u16_(14),
            cb: u16_(16),
            valid: u16_(18),
            sub: (f[28..40] == [0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71]).then(|| u32_(24)),
        };
        assert_eq!((w.tag, w.cb, w.sub, u32_(20)), (0xfffe, 22, Some(1), 3));
        assert_eq!(crate::wasapi::shared_supported(&w), 0);
    }
}
