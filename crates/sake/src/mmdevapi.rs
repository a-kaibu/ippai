//! mmdevapi: the multimedia device enumerator (CLSID_MMDeviceEnumerator) with one render
//! endpoint, the browser's audio output, and no capture endpoints. Games ask it for the default
//! device and register for device-change notifications (AdvHD dereferences the enumerator
//! without checking CoCreateInstance); the sound itself goes through XAudio2 / DirectSound.
//! Objects are stateless singletons (their reference counts do not matter).

use std::sync::OnceLock;

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
const DEVICE_ID: &str = "{0.0.0.00000000}.{5a2c1e0b-7f3d-4e8a-9c61-3b8d0f4a2e17}";
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
/// GetDevice(id, IMMDevice**)
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
        E_NOTFOUND
    }
}
/// Register / UnregisterEndpointNotificationCallback: the one device never changes
fn en_notify(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 { E_POINTER } else { S_OK }
}
pub fn enumerator() -> u32 {
    static O: OnceLock<u32> = OnceLock::new();
    singleton("IMMDeviceEnumerator", &[(en_qi, b(3)), (one, b(1)), (one, b(1)), (en_enum, b(4)), (en_default, b(4)), (en_get, b(3)), (en_notify, b(2)), (en_notify, b(2))], &O)
}

// ---------------------------------------------------------------------------------------------
// IMMDeviceCollection {0bd7a1be-7a1a-44db-8397-cc5392387b5e}: none or the one device

fn col_qi(c: &mut Ctx) -> u32 {
    qi(c, (0x0bd7_a1be, 0x7a1a, 0x44db))
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
    static O: [OnceLock<u32>; 2] = [OnceLock::new(), OnceLock::new()];
    let o = singleton("IMMDeviceCollection", &[(col_qi, b(3)), (one, b(1)), (one, b(1)), (col_count, b(2)), (col_item, b(3))], &O[has as usize]);
    w32(o + ps(), has as u32);
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
/// Activate(iid, clsctx, params, void**): no audio client of its own (games here play through
/// XAudio2 / DirectSound); the property store, though
fn dev_activate(c: &mut Ctx) -> u32 {
    let (iid, out) = (c.a(1), c.a(4));
    if iid == 0 || out == 0 {
        return E_POINTER;
    }
    if guid_is(iid, 0x886d_8eeb, 0x8cf2, 0x4446) {
        putp(out, store());
        return S_OK;
    }
    crate::com::warn_once(&format!("IMMDevice::Activate {:08x}: not available", r32(iid)));
    putp(out, 0);
    E_NOINTERFACE
}
/// OpenPropertyStore(access, IPropertyStore**)
fn dev_store(c: &mut Ctx) -> u32 {
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

/// The keys answered: PKEY_Device_FriendlyName, PKEY_Device_DeviceDesc,
/// PKEY_DeviceInterface_FriendlyName
const KEYS: [((u32, u16, u16), u32, &str); 3] = [
    ((0xa45c_254e, 0xdf1c, 0x4efd), 14, FRIENDLY_NAME),
    ((0xa45c_254e, 0xdf1c, 0x4efd), 2, "スピーカー"),
    ((0x026e_516e, 0xb814, 0x414b), 2, "sake Audio"),
];
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
    let Some(((d1, d2, d3), pid, _)) = KEYS.get(i) else { return E_INVALIDARG };
    // (the rest of the GUID: Data4)
    let d4: [u8; 8] = match d1 {
        0xa45c_254e => [0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0],
        _ => [0x83, 0xcd, 0x85, 0x6d, 0x6f, 0xef, 0x48, 0x22],
    };
    w32(key, *d1);
    w16(key + 4, *d2);
    w16(key + 6, *d3);
    mem::write(key + 8, &d4);
    w32(key + 16, *pid);
    S_OK
}
/// GetValue(REFPROPERTYKEY, PROPVARIANT*): VT_LPWSTR for the names, VT_EMPTY otherwise
fn ps_get(c: &mut Ctx) -> u32 {
    let (key, v) = (c.a(1), c.a(2));
    if key == 0 || v == 0 {
        return E_POINTER;
    }
    mem::fill(v, 8 + 2 * ps(), 0);
    let pid = r32(key + 16);
    if let Some((_, _, s)) = KEYS.iter().find(|((d1, d2, d3), p, _)| guid_is(key, *d1, *d2, *d3) && *p == pid) {
        w16(v, 31);
        putp(v + 8, task_wstr(s));
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

