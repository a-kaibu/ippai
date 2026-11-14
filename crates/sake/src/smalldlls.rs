//! Small system DLLs games import or load that a browser has nothing behind, answered as a
//! Windows machine without the hardware or service answers (they used to come from a Wine
//! prefix): dnsapi (name rules, IP literals; no DNS server, so other queries time out),
//! dhcpcsvc (no DHCP adapter), tbs (no TPM), ncrypt (the software key storage provider opens;
//! keys are not supported), dxva2 (the Direct3D 9 device manager, monitor configuration; no video
//! services), ksuser (no kernel-streaming devices), msdmo (an in-process DMO registry and the
//! media type helpers), hhctrl (no HTML Help), ETW providers (advapi32 / ntdll Event*: they
//! register, nothing consumes their events).
//!
//! Behaviour follows Wine's conformance tests (dlls/<dll>/tests, Windows' answers), and the pure
//! parts are tested below with their tables.

#![allow(non_snake_case)]

use std::sync::Mutex;

use crate::api::{implement, Ctx};
use crate::dnsrec::{self, Data, Rec};
use crate::k32::last_error;
use crate::mem::{self, cstr, ps, put32, putp, r16, r32, rp, w16, w32, wp, wstr};

const E_INVALIDARG: u32 = 0x8007_0057;
const E_POINTER: u32 = 0x8000_4003;
const E_FAIL: u32 = 0x8000_4005;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_HANDLE: u32 = 0x8007_0006;
const E_OUTOFMEMORY: u32 = 0x8007_000e;
const S_FALSE: u32 = 1;
const ERROR_INVALID_PARAMETER: u32 = 87;

fn guid(p: u32) -> [u8; 16] {
    let mut g = [0u8; 16];
    g.copy_from_slice(mem::slice(p, 16));
    g
}
fn put_guid(p: u32, g: &[u8; 16]) {
    mem::write(p, g);
}

// ---------------------------------------------------------------------------------------------
// dnsapi

pub const ERROR_INVALID_NAME: u32 = 123;
pub const DNS_ERROR_NON_RFC_NAME: u32 = 9556;
pub const DNS_ERROR_INVALID_NAME_CHAR: u32 = 9560;
pub const DNS_ERROR_NUMERIC_NAME: u32 = 9561;
const ERROR_TIMEOUT: u32 = 1460;

/// DNS_NAME_FORMAT
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum NameFormat {
    Domain = 0,
    DomainLabel = 1,
    HostnameFull = 2,
    HostnameLabel = 3,
    Wildcard = 4,
    SrvRecord = 5,
}

/// DnsValidateName: the rules as Windows answers them (Wine's dnsapi tests' table; Windows 10
/// and later accept all-numeric names and labels).
pub fn validate_name(name: &[u16], format: u32) -> u32 {
    const EXTENDED: u32 = 1;
    const NUMERIC: u32 = 2;
    const NON_NUMERIC: u32 = 4;
    const DOT: u32 = 8;
    const DOT_DOT: u32 = 0x10;
    const SPACE: u32 = 0x20;
    const INVALID: u32 = 0x40;
    const ASTERISK: u32 = 0x80;
    const UNDERSCORE: u32 = 0x100;
    const LONG_LABEL: u32 = 0x200;
    let invalid = "{|}~[\\]^':;<=>?@!\"#$%`()+/,";
    let mut state = 0;
    let mut j = 0;
    for (i, &c) in name.iter().enumerate() {
        if c == b'.' as u16 {
            j = 0;
            state |= DOT;
            if name.get(i + 1) == Some(&(b'.' as u16)) {
                state |= DOT_DOT;
            }
        } else if (b'0' as u16..=b'9' as u16).contains(&c) {
            state |= NUMERIC;
        } else {
            state |= NON_NUMERIC;
        }
        if j > 62 {
            state |= LONG_LABEL;
        }
        if c < 128 && invalid.as_bytes().contains(&(c as u8)) {
            state |= INVALID;
        } else if c > 127 {
            state |= EXTENDED;
        } else if c == b' ' as u16 {
            state |= SPACE;
        } else if c == b'_' as u16 {
            state |= UNDERSCORE;
        } else if c == b'*' as u16 {
            state |= ASTERISK;
        }
        j += 1;
    }
    let n = name.len();
    if n == 0 || n > 255 || state & (LONG_LABEL | DOT_DOT) != 0 || (name[0] == b'.' as u16 && n > 1) {
        return ERROR_INVALID_NAME;
    }
    let numeric_only = state & NON_NUMERIC == 0 && state & NUMERIC != 0;
    let bad_char = state & (SPACE | INVALID | ASTERISK) != 0;
    let non_rfc = state & (EXTENDED | UNDERSCORE) != 0;
    match format {
        0 | 2 => {
            // (Domain, HostnameFull)
            if non_rfc && !bad_char {
                return DNS_ERROR_NON_RFC_NAME;
            }
            if bad_char {
                return DNS_ERROR_INVALID_NAME_CHAR;
            }
        }
        1 | 3 => {
            // (DomainLabel, HostnameLabel)
            if state & DOT != 0 {
                return ERROR_INVALID_NAME;
            }
            if non_rfc && !bad_char {
                return DNS_ERROR_NON_RFC_NAME;
            }
            if bad_char {
                return DNS_ERROR_INVALID_NAME_CHAR;
            }
        }
        4 => {
            if numeric_only || name[0] != b'*' as u16 {
                return ERROR_INVALID_NAME;
            }
            if n > 1 && name[1] != b'.' as u16 {
                return DNS_ERROR_INVALID_NAME_CHAR;
            }
            if state & (EXTENDED | SPACE | INVALID) != 0 {
                return ERROR_INVALID_NAME;
            }
        }
        5 => {
            if numeric_only || name[0] != b'_' as u16 {
                return ERROR_INVALID_NAME;
            }
            if n == 1 {
                return DNS_ERROR_NON_RFC_NAME;
            }
            // (only the first label, the service, starts with '_')
            if name.windows(2).any(|w| w[0] == b'.' as u16 && w[1] == b'_' as u16) || state & (EXTENDED | SPACE | INVALID) != 0 {
                return ERROR_INVALID_NAME;
            }
        }
        _ => {}
    }
    0
}

/// DnsNameCompare: case-insensitive, trailing dots ignored; two NULLs are equal
pub fn names_equal(a: Option<&[u16]>, b: Option<&[u16]>) -> bool {
    let (Some(a), Some(b)) = (a, b) else { return a.is_none() && b.is_none() };
    let trim = |s: &[u16]| -> Vec<u16> {
        let mut v = s.to_vec();
        while v.last() == Some(&(b'.' as u16)) {
            v.pop();
        }
        v.iter().map(|&c| if (b'a' as u16..=b'z' as u16).contains(&c) { c - 32 } else { c }).collect()
    };
    trim(a) == trim(b)
}

/// An IPv4 or IPv6 literal (what DnsQuery answers itself), as its address bytes
pub fn ip_literal(s: &str) -> Option<Vec<u8>> {
    if let Ok(a) = s.parse::<std::net::Ipv4Addr>() {
        return Some(a.octets().to_vec());
    }
    s.parse::<std::net::Ipv6Addr>().ok().map(|a| a.octets().to_vec())
}

/// DnsQuery_A / _W / _UTF8(name, type, options, extra, ppResults, reserved)
fn dns_query(c: &mut Ctx, charset: u32) -> u32 {
    let (np, ty, out) = (c.a(0), c.a(1) & 0xffff, c.a(4));
    if out != 0 {
        putp(out, 0);
    }
    if np == 0 || out == 0 {
        return 87;
    }
    let name: Vec<u16> = if charset == 1 { wstr(np) } else { String::from_utf8_lossy(&cstr(np)).encode_utf16().collect() };
    let s = String::from_utf16_lossy(&name);
    if s.starts_with(' ') || s.ends_with(' ') {
        return DNS_ERROR_INVALID_NAME_CHAR;
    }
    // the resolver answers IP literals and localhost itself
    let local = names_equal(Some(&name), Some(&"localhost".encode_utf16().collect::<Vec<u16>>()));
    let ip = match ip_literal(&s) {
        Some(ip) => ip,
        None if local && ty == 28 => std::net::Ipv6Addr::LOCALHOST.octets().to_vec(),
        None if local => vec![127, 0, 0, 1],
        // (no DNS server answers here)
        None => return ERROR_TIMEOUT,
    };
    // a record for the address: A (1) for IPv4, AAAA (28) for IPv6
    let want = if ip.len() == 4 { 1 } else { 28 };
    if ty != want {
        return 9501; // DNS_INFO_NO_RECORDS
    }
    let data = if ip.len() == 4 { Data::A(ip[..4].try_into().unwrap()) } else { Data::Aaaa(ip[..16].try_into().unwrap()) };
    // Flags: section 0, the character set, Reserved 0x20 (as Windows sets them)
    let r = Rec { name: Some(s), ty: want as u16, data_len: ip.len() as u16, flags: 0x20 << 8, ttl: 604800, reserved: 0, data };
    putp(out, dnsrec::write(&r, charset));
    0
}
/// DnsRecordListFree(list, type) / DnsFree(p, type): each record is one block (its strings in
/// it), so a record list frees block by block.
fn dns_free(c: &mut Ctx) -> u32 {
    let (p, ty) = (c.a(0), c.a(1));
    if ty == 1 {
        // DnsFreeRecordList
        for r in dnsrec::list(p) {
            mem::heap_free(r);
        }
    } else if p != 0 {
        mem::heap_free(p);
    }
    0
}

/// DnsExtractRecordsFromMessage_UTF8 / _W(buffer, length, ppRecord)
fn dns_extract(c: &mut Ctx, charset: u32) -> u32 {
    let (b, len, out) = (c.a(0), c.a(1) & 0xffff, c.a(2));
    if b == 0 || out == 0 {
        return 87;
    }
    match dnsrec::extract(mem::slice(b, len), charset) {
        Ok(v) => {
            let ps: Vec<u32> = v.iter().map(|r| dnsrec::write(r, charset)).collect();
            putp(out, dnsrec::link(&ps));
            0
        }
        Err(e) => e,
    }
}

fn set_copy(set: u32, cin: u32, cout: u32) -> u32 {
    let v: Vec<u32> = dnsrec::list(set).into_iter().map(|r| dnsrec::copy(r, cin, cout)).collect();
    dnsrec::link(&v)
}

/// DnsRecordCompare(r1, r2): the strings read as r1's character set says
fn record_compare(a: u32, b: u32) -> bool {
    let wide = dnsrec::wide(dnsrec::charset_of(a, 0));
    dnsrec::compare(&dnsrec::read(a, wide), &dnsrec::read(b, wide))
}

/// DnsRecordSetCompare(set1, set2, ppDiff1, ppDiff2): copies of the records of each set some
/// record of the other differs from
fn record_set_compare(c: &mut Ctx) -> u32 {
    let (s1, s2, d1, d2) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if s1 == 0 && s2 == 0 {
        return 0;
    }
    for d in [d1, d2] {
        if d != 0 {
            putp(d, 0);
        }
    }
    let cs = |p: u32| dnsrec::charset_of(p, 0);
    if s2 == 0 {
        if d1 != 0 {
            putp(d1, set_copy(s1, cs(s1), cs(s1)));
        }
        return 0;
    }
    if s1 == 0 {
        if d2 != 0 {
            putp(d2, set_copy(s2, cs(s2), cs(s2)));
        }
        return 0;
    }
    let diff = |a: u32, b: u32| -> Vec<u32> {
        let mut v = Vec::new();
        for r in dnsrec::list(a) {
            for t in dnsrec::list(b) {
                let u = dnsrec::copy(r, cs(r), cs(t));
                if record_compare(t, u) {
                    mem::heap_free(u);
                } else {
                    v.push(u);
                }
            }
        }
        v
    };
    let (x, y) = (diff(s1, s2), diff(s2, s1));
    let same = x.is_empty() && y.is_empty();
    for (d, v) in [(d1, x), (d2, y)] {
        if d != 0 {
            putp(d, dnsrec::link(&v));
        } else {
            v.into_iter().for_each(|p| {
                mem::heap_free(p);
            });
        }
    }
    same as u32
}

/// DnsRecordSetDetach(set): cuts the last record off, returns it
fn record_set_detach(c: &mut Ctx) -> u32 {
    let v = dnsrec::list(c.a(0));
    if v.len() < 2 {
        return 0;
    }
    wp(v[v.len() - 2], 0);
    v[v.len() - 1]
}

/// DnsGetCacheDataTable(ppEntry): the resolver cache holds localhost
fn cache_table(c: &mut Ctx) -> u32 {
    let out = c.a(0);
    if out == 0 {
        return 0;
    }
    let k = ps();
    let e = mem::heap_alloc(2 * k + 8 + 20, true);
    let w: Vec<u16> = "localhost".encode_utf16().collect();
    mem::put_wstr(e + 2 * k + 8, 10, &w);
    wp(e + k, e + 2 * k + 8);
    w16(e + 2 * k, 1);
    w16(e + 2 * k + 2, 4);
    putp(out, e);
    1
}

fn dup_wstr(p: u32) -> u32 {
    if p == 0 {
        return 0;
    }
    let s = wstr(p);
    let q = mem::heap_alloc(2 * (s.len() as u32 + 1), true);
    for (i, &u) in s.iter().enumerate() {
        w16(q + 2 * i as u32, u);
    }
    q
}

/// The DNS_SERVICE_INSTANCE field offsets: (wPort, dwPropertyCount, keys, size)
fn svc_off() -> (u32, u32, u32, u32) {
    let k = ps();
    let keys = (4 * k + 12).next_multiple_of(k);
    (4 * k, 4 * k + 8, keys, (keys + 2 * k + 4).next_multiple_of(k))
}

/// The most properties a service instance takes here
const SERVICE_PROPS_MAX: u32 = 0x1000;

/// DnsServiceConstructInstance(name, host, ip4, ip6, port, priority, weight, count, keys, values)
fn service_construct(c: &mut Ctx) -> u32 {
    let k = ps();
    // (a wild property count: k * n wrapped and the loop below ran for ever)
    if c.a(7) > SERVICE_PROPS_MAX {
        crate::k32::last_error(87);
        return 0;
    }
    let (port, count, keys, size) = svc_off();
    let o = mem::heap_alloc(size, true);
    wp(o, dup_wstr(c.a(0)));
    wp(o + k, dup_wstr(c.a(1)));
    for (i, n) in [(2, 4u32), (3, 16)] {
        if c.a(i) != 0 {
            let q = mem::heap_alloc(n, false);
            mem::write(q, &mem::slice(c.a(i), n).to_vec());
            wp(o + k * i as u32, q);
        }
    }
    w16(o + port, c.a(4) as u16);
    w16(o + port + 2, c.a(5) as u16);
    w16(o + port + 4, c.a(6) as u16);
    let n = c.a(7);
    w32(o + count, n);
    for (j, src) in [(0, c.a(8)), (1, c.a(9))] {
        if n != 0 && src != 0 {
            let arr = mem::heap_alloc(k * n, true);
            for i in 0..n {
                wp(arr + k * i, dup_wstr(rp(src + k * i)));
            }
            wp(o + keys + k * j, arr);
        }
    }
    o
}

fn service_free(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    if o == 0 {
        return 0;
    }
    let k = ps();
    let (_, count, keys, _) = svc_off();
    let free = |p: u32| {
        if p != 0 {
            mem::heap_free(p);
        }
    };
    for i in 0..4 {
        free(rp(o + k * i));
    }
    for j in 0..2 {
        let arr = rp(o + keys + k * j);
        if arr != 0 {
            for i in 0..r32(o + count) {
                free(rp(arr + k * i));
            }
            mem::heap_free(arr);
        }
    }
    mem::heap_free(o);
    0
}

// ---------------------------------------------------------------------------------------------
// dxva2: IDirect3DDeviceManager9

const DXVA2_E_NOT_INITIALIZED: u32 = 0x8004_1000;
const DXVA2_E_NEW_VIDEO_DEVICE: u32 = 0x8004_1001;
const DXVA2_E_VIDEO_DEVICE_LOCKED: u32 = 0x8004_1002;

#[derive(Default)]
struct Manager {
    obj: u32,
    refs: u32,
    token: u32,
    device: u32,
    /// open handles: Some(stale after a reset)
    handles: Vec<Option<bool>>,
    /// the handle holding the lock (1-based), 0 none
    locked: u32,
    /// its video processor service (made on the first GetVideoService)
    service: u32,
    /// state blocks UnlockDevice(fSaveState) kept, by handle; the next LockDevice applies them
    saved: Vec<(u32, u32)>,
}
static MANAGERS: Mutex<Vec<Manager>> = Mutex::new(Vec::new());

fn with_mgr<R>(this: u32, f: impl FnOnce(&mut Manager) -> R) -> Option<R> {
    MANAGERS.lock().unwrap().iter_mut().find(|m| m.obj == this).map(f)
}

/// IUnknown::AddRef / Release of a guest object
fn add_ref(c: &mut Ctx, o: u32) {
    let f = rp(rp(o) + ps());
    c.call(f, &[o]);
}
fn release(c: &mut Ctx, o: u32) {
    let f = rp(rp(o) + 2 * ps());
    c.call(f, &[o]);
}

fn DM_QueryInterface(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    // IUnknown, IDirect3DDeviceManager9 {a0cade0f-06d5-4cf4-a1c7-f3cdd725aa75}
    if matches!(r32(iid), 0 | 0xa0ca_de0f) {
        with_mgr(this, |m| m.refs += 1);
        putp(out, this);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn DM_AddRef(c: &mut Ctx) -> u32 {
    with_mgr(c.a(0), |m| {
        m.refs += 1;
        m.refs
    })
    .unwrap_or(0)
}
fn DM_Release(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let (n, dev, svc) = with_mgr(this, |m| {
        m.refs = m.refs.saturating_sub(1);
        (m.refs, m.device, m.service)
    })
    .unwrap_or((0, 0, 0));
    if n == 0 {
        // (the state blocks UnlockDevice kept are the device's objects: released, not dropped)
        let saved = with_mgr(this, |m| std::mem::take(&mut m.saved)).unwrap_or_default();
        MANAGERS.lock().unwrap().retain(|m| m.obj != this);
        for (_, sb) in saved {
            release(c, sb);
        }
        if svc != 0 {
            mem::heap_free(svc);
        }
        if dev != 0 {
            release(c, dev);
        }
    }
    n
}
/// ResetDevice(device, token): a new device; every open handle is then stale
fn DM_ResetDevice(c: &mut Ctx) -> u32 {
    let (this, dev, token) = (c.a(0), c.a(1), c.a(2));
    let Some(ok) = with_mgr(this, |m| m.token == token) else { return E_INVALIDARG };
    if !ok || dev == 0 {
        return E_INVALIDARG;
    }
    add_ref(c, dev);
    let old = with_mgr(this, |m| {
        for h in m.handles.iter_mut().flatten() {
            *h = true;
        }
        m.locked = 0;
        std::mem::replace(&mut m.device, dev)
    })
    .unwrap_or(0);
    if old != 0 {
        release(c, old);
    }
    0
}
fn DM_OpenDeviceHandle(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let r = with_mgr(this, |m| {
        if m.device == 0 {
            return Err(DXVA2_E_NOT_INITIALIZED);
        }
        let i = m.handles.iter().position(|h| h.is_none()).unwrap_or_else(|| {
            m.handles.push(None);
            m.handles.len() - 1
        });
        m.handles[i] = Some(false);
        Ok(i as u32 + 1)
    });
    match r {
        Some(Ok(h)) => {
            putp(out, h);
            0
        }
        Some(Err(e)) => {
            putp(out, 0);
            e
        }
        None => E_POINTER,
    }
}
fn handle_state(m: &Manager, h: u32) -> Option<bool> {
    if h == 0 {
        return None;
    }
    m.handles.get(h as usize - 1).copied().flatten()
}
fn DM_CloseDeviceHandle(c: &mut Ctx) -> u32 {
    let (this, h) = (c.a(0), c.a(1));
    with_mgr(this, |m| {
        if handle_state(m, h).is_none() {
            return E_HANDLE;
        }
        m.handles[h as usize - 1] = None;
        if m.locked == h {
            m.locked = 0;
        }
        0
    })
    .unwrap_or(E_HANDLE)
}
fn DM_TestDevice(c: &mut Ctx) -> u32 {
    let (this, h) = (c.a(0), c.a(1));
    with_mgr(this, |m| match handle_state(m, h) {
        None => E_HANDLE,
        Some(true) => DXVA2_E_NEW_VIDEO_DEVICE,
        Some(false) => 0,
    })
    .unwrap_or(E_HANDLE)
}
/// LockDevice(handle, ppDevice, fBlock): one handle holds the device at a time (another's lock
/// fails with DXVA2_E_VIDEO_DEVICE_LOCKED; sake does not wait for it)
fn DM_LockDevice(c: &mut Ctx) -> u32 {
    let (this, h, out) = (c.a(0), c.a(1), c.a(2));
    let r = with_mgr(this, |m| {
        if m.device == 0 {
            return Err(DXVA2_E_NOT_INITIALIZED);
        }
        match handle_state(m, h) {
            None => return Err(E_HANDLE),
            Some(true) => return Err(DXVA2_E_NEW_VIDEO_DEVICE),
            Some(false) => {}
        }
        if m.locked != 0 && m.locked != h {
            return Err(DXVA2_E_VIDEO_DEVICE_LOCKED);
        }
        m.locked = h;
        Ok(m.device)
    })
    .unwrap_or(Err(E_HANDLE));
    match r {
        Ok(dev) => {
            let sb = with_mgr(this, |m| m.saved.iter().position(|s| s.0 == h).map(|i| m.saved.remove(i).1)).flatten();
            if let Some(sb) = sb {
                com_call(c, sb, 5, &[]); // IDirect3DStateBlock9::Apply
                release(c, sb);
            }
            add_ref(c, dev);
            if out != 0 {
                putp(out, dev);
            }
            0
        }
        Err(e) => {
            if out != 0 {
                putp(out, 0);
            }
            e
        }
    }
}
/// UnlockDevice(handle, fSaveState): with fSaveState the device's state is kept (a D3DSBT_ALL
/// state block) and given back by the handle's next LockDevice
fn DM_UnlockDevice(c: &mut Ctx) -> u32 {
    let (this, h, save) = (c.a(0), c.a(1), c.a(2));
    let r = with_mgr(this, |m| {
        if h == 0 || m.locked != h {
            return Err(E_INVALIDARG);
        }
        m.locked = 0;
        Ok(m.device)
    })
    .unwrap_or(Err(E_INVALIDARG));
    let dev = match r {
        Ok(d) => d,
        Err(e) => return e,
    };
    if save != 0 && dev != 0 {
        let out = mem::heap_alloc(ps(), true);
        if com_call(c, dev, 59, &[1, out]) == 0 {
            let sb = rp(out);
            let old = with_mgr(this, |m| {
                let old = m.saved.iter().position(|s| s.0 == h).map(|i| m.saved.remove(i).1);
                m.saved.push((h, sb));
                old
            })
            .flatten();
            if let Some(o) = old {
                release(c, o);
            }
        }
        mem::heap_free(out);
    }
    0
}

/// Method `slot` of the guest COM object `o`
fn com_call(c: &mut Ctx, o: u32, slot: u32, args: &[u32]) -> u32 {
    let f = rp(rp(o) + slot * ps());
    let mut a = vec![o];
    a.extend_from_slice(args);
    c.call(f, &a)
}
/// GetVideoService(handle, riid, ppService): the manager's video processor service (one per
/// manager; it works on the manager's current device)
fn DM_GetVideoService(c: &mut Ctx) -> u32 {
    let (this, h, iid, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if out == 0 {
        return E_POINTER;
    }
    putp(out, 0);
    let r = with_mgr(this, |m| match handle_state(m, h) {
        None => Err(E_HANDLE),
        Some(true) => Err(DXVA2_E_NEW_VIDEO_DEVICE),
        Some(false) => Ok(m.service),
    })
    .unwrap_or(Err(E_HANDLE));
    let svc = match r {
        Ok(0) => {
            let o = new_service(0, this);
            with_mgr(this, |m| m.service = o);
            o
        }
        Ok(o) => o,
        Err(e) => return e,
    };
    service_qi(svc, iid, out)
}

// ---- dxva2 video services: the software video processor (DXVA2_VideoProcSoftwareDevice) ----

const DXVA2_SOFTWARE_DEVICE: [u8; 16] = [0x7f, 0xd4, 0x53, 0x45, 0x7e, 0xee, 0x3f, 0x4e, 0x94, 0x75, 0xdb, 0xf1, 0x37, 0x6c, 0x48, 0x10];
const D3DFMT_X8R8G8B8: u32 = 22;
const D3DFMT_A8R8G8B8: u32 = 21;
/// what the software processor takes: A8R8G8B8, X8R8G8B8, YUY2, AYUV
const SOFTWARE_INPUTS: [u32; 4] = [21, 22, 0x3259_5559, 0x5655_5941];

/// service object: vtbl, refs, device (a service of its own), manager (a manager's)
fn svc_device(o: u32) -> u32 {
    let m = rp(o + 2 * ps() + 4);
    if m != 0 {
        with_mgr(m, |m| m.device).unwrap_or(0)
    } else {
        rp(o + ps() + 4)
    }
}

fn new_service(device: u32, manager: u32) -> u32 {
    static VT: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *VT.get_or_init(|| {
        let m: [(crate::api::Handler, u32); 12] = [
            (VS_QueryInterface, 12),
            (VS_AddRef, 4),
            (VS_Release, 4),
            (VS_CreateSurface, 40),
            (|_| 0x8000_4001, 8), // RegisterVideoProcessorSoftwareDevice: E_NOTIMPL
            (VS_GetDeviceGuids, 16),
            (VS_GetRenderTargets, 20),
            (VS_GetSubStreamFormats, 24),
            (VS_GetCaps, 20),
            (|_| 0x8000_4001, 24), // GetProcAmpRange: no ProcAmp controls
            (|_| 0x8000_4001, 24), // GetFilterPropertyRange: no filters
            (VS_CreateVideoProcessor, 24),
        ];
        crate::api::vtable("IDirectXVideoProcessorService", &m)
    });
    let k = ps();
    let o = mem::heap_alloc(3 * k + 4, true);
    wp(o, vt);
    w32(o + k, 1);
    wp(o + k + 4, device);
    wp(o + 2 * k + 4, manager);
    o
}

fn service_qi(o: u32, iid: u32, out: u32) -> u32 {
    let g = guid(iid);
    let unknown = g == [0, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
    // IDirectXVideoAccelerationService {fc51a550-d5e7-11d9-af55-00054e43ff02}, the processor
    // service {fc51a552-...}; (no decoder service, fc51a551: sake has no video decoders)
    let tail = [0xe7, 0xd5, 0xd9, 0x11, 0xaf, 0x55, 0x00, 0x05, 0x4e, 0x43, 0xff, 0x02];
    if unknown || (matches!(r32(iid), 0xfc51_a550 | 0xfc51_a552) && g[4..] == tail) {
        vs_ref(o, 1);
        putp(out, o);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
/// a manager's service counts on the manager (as Windows': AddRef/Release go to it)
fn vs_ref(o: u32, d: i32) -> u32 {
    let m = rp(o + 2 * ps() + 4);
    if m != 0 {
        return with_mgr(m, |m| {
            m.refs = (m.refs as i32 + d).max(0) as u32;
            m.refs
        })
        .unwrap_or(0);
    }
    let n = (r32(o + ps()) as i32 + d).max(0) as u32;
    w32(o + ps(), n);
    n
}
fn VS_QueryInterface(c: &mut Ctx) -> u32 {
    service_qi(c.a(0), c.a(1), c.a(2))
}
fn VS_AddRef(c: &mut Ctx) -> u32 {
    vs_ref(c.a(0), 1)
}
fn VS_Release(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    let m = rp(o + 2 * ps() + 4);
    if m != 0 {
        // (through the manager's own Release, which frees it at 0)
        let f = rp(rp(m) + 2 * ps());
        return c.call(f, &[m]);
    }
    let n = vs_ref(o, -1);
    if n == 0 {
        let dev = rp(o + ps() + 4);
        if dev != 0 {
            release(c, dev);
        }
        mem::heap_free(o);
    }
    n
}
/// CreateSurface(width, height, backbuffers, format, pool, usage, type, ppSurface, pShared):
/// backbuffers + 1 offscreen plain surfaces of the device
fn VS_CreateSurface(c: &mut Ctx) -> u32 {
    let (o, w, h, back, fmt, pool, out, shared) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(8), c.a(9));
    let dev = svc_device(o);
    if out == 0 || dev == 0 || back > 64 {
        return E_INVALIDARG;
    }
    let create = rp(rp(dev) + 36 * ps()); // IDirect3DDevice9::CreateOffscreenPlainSurface
    for i in 0..=back {
        let hr = c.call(create, &[dev, w, h, fmt, pool, out + ps() * i, shared]);
        if hr & 0x8000_0000 != 0 {
            for j in 0..i {
                release(c, rp(out + ps() * j));
                wp(out + ps() * j, 0);
            }
            return hr;
        }
    }
    0
}
/// DXVA2_VideoDesc.Format
fn desc_format(d: u32) -> u32 {
    if d == 0 {
        0
    } else {
        r32(d + 12)
    }
}
fn task_mem(words: &[u32]) -> u32 {
    let p = mem::heap_alloc(4 * words.len() as u32, false);
    for (i, &w) in words.iter().enumerate() {
        w32(p + 4 * i as u32, w);
    }
    p
}
/// GetVideoProcessorDeviceGuids(desc, pCount, ppGuids)
fn VS_GetDeviceGuids(c: &mut Ctx) -> u32 {
    let (d, count, out) = (c.a(1), c.a(2), c.a(3));
    if count == 0 || out == 0 {
        return E_POINTER;
    }
    put32(count, 0);
    putp(out, 0);
    if !SOFTWARE_INPUTS.contains(&desc_format(d)) {
        return E_FAIL;
    }
    let p = mem::heap_alloc(16, false);
    put_guid(p, &DXVA2_SOFTWARE_DEVICE);
    put32(count, 1);
    putp(out, p);
    0
}
fn is_software(g: u32) -> bool {
    g != 0 && guid(g) == DXVA2_SOFTWARE_DEVICE
}
/// GetVideoProcessorRenderTargets(device, desc, pCount, ppFormats)
fn VS_GetRenderTargets(c: &mut Ctx) -> u32 {
    let (g, d, count, out) = (c.a(1), c.a(2), c.a(3), c.a(4));
    if count == 0 || out == 0 {
        return E_POINTER;
    }
    if !is_software(g) || !SOFTWARE_INPUTS.contains(&desc_format(d)) {
        return E_FAIL;
    }
    put32(count, 2);
    putp(out, task_mem(&[D3DFMT_X8R8G8B8, D3DFMT_A8R8G8B8]));
    0
}
/// GetVideoProcessorSubStreamFormats(device, desc, rtFormat, pCount, ppFormats)
fn VS_GetSubStreamFormats(c: &mut Ctx) -> u32 {
    let (g, d, count, out) = (c.a(1), c.a(2), c.a(4), c.a(5));
    if count == 0 || out == 0 {
        return E_POINTER;
    }
    if !is_software(g) || !SOFTWARE_INPUTS.contains(&desc_format(d)) {
        return E_FAIL;
    }
    put32(count, 2);
    putp(out, task_mem(&[D3DFMT_A8R8G8B8, 0x5655_5941]));
    0
}
/// DXVA2_VideoProcessorCaps of the software device
fn software_caps(p: u32) {
    // SoftwareDevice, D3DPOOL_SYSTEMMEM, no reference samples, Reserved, no deinterlacing, no
    // ProcAmp; YUV2RGB StretchX StretchY SubRects SubStreams SubStreamsExtended YUV2RGBExtended
    // PlanarAlpha; no noise or detail filters
    let caps = [4, 2, 0, 0, 0, 0, 0, 0x1000 | 0x1 | 0x2 | 0x4 | 0x10 | 0x20 | 0x40 | 0x80, 0, 0];
    for (i, v) in caps.iter().enumerate() {
        w32(p + 4 * i as u32, *v);
    }
}
fn VS_GetCaps(c: &mut Ctx) -> u32 {
    let (g, caps) = (c.a(1), c.a(4));
    if caps == 0 {
        return E_POINTER;
    }
    if !is_software(g) {
        return E_FAIL;
    }
    software_caps(caps);
    0
}
/// processor object: vtbl, refs, service, device GUID (16), DXVA2_VideoDesc (36), rtFormat,
/// max substreams
fn vp_fields() -> (u32, u32, u32, u32, u32) {
    let k = ps();
    let g = 2 * k + 4;
    (k + 4, g, g + 16, g + 52, g + 56)
}
/// CreateVideoProcessor(device, desc, rtFormat, maxSubStreams, ppProcessor)
fn VS_CreateVideoProcessor(c: &mut Ctx) -> u32 {
    let (o, g, d, rt, max, out) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    if out == 0 || d == 0 {
        return E_POINTER;
    }
    putp(out, 0);
    if !is_software(g) {
        return E_INVALIDARG;
    }
    // (substreams beside the reference stream: at most 15)
    if max >= 16 || !SOFTWARE_INPUTS.contains(&desc_format(d)) || ![D3DFMT_X8R8G8B8, D3DFMT_A8R8G8B8].contains(&rt) {
        return E_INVALIDARG;
    }
    static VT: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *VT.get_or_init(|| {
        let m: [(crate::api::Handler, u32); 9] = [
            (VP_QueryInterface, 12),
            (VP_AddRef, 4),
            (VP_Release, 4),
            (VP_GetVideoProcessorService, 8),
            (VP_GetCreationParameters, 20),
            (VP_GetVideoProcessorCaps, 8),
            (|_| 0x8000_4001, 12), // GetProcAmpRange: no ProcAmp controls
            (|_| 0x8000_4001, 12), // GetFilterPropertyRange: no filters
            (VP_VideoProcessBlt, 24),
        ];
        crate::api::vtable("IDirectXVideoProcessor", &m)
    });
    let (svc, gd, desc, rtf, mx) = vp_fields();
    let p = mem::heap_alloc(mx + 4, true);
    wp(p, vt);
    w32(p + ps(), 1);
    vs_ref(o, 1);
    wp(p + svc, o);
    put_guid(p + gd, &guid(g));
    mem::write(p + desc, &mem::slice(d, 36).to_vec());
    w32(p + rtf, rt);
    w32(p + mx, max);
    putp(out, p);
    0
}
fn VP_QueryInterface(c: &mut Ctx) -> u32 {
    let (o, iid, out) = (c.a(0), c.a(1), c.a(2));
    // IUnknown, IDirectXVideoProcessor {8c3a39f0-916e-4690-804f-4c8001355d25}
    let g = guid(iid);
    if g == [0, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46] || g == [0xf0, 0x39, 0x3a, 0x8c, 0x6e, 0x91, 0x90, 0x46, 0x80, 0x4f, 0x4c, 0x80, 0x01, 0x35, 0x5d, 0x25] {
        w32(o + ps(), r32(o + ps()) + 1);
        putp(out, o);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn VP_AddRef(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    w32(o + ps(), r32(o + ps()) + 1);
    r32(o + ps())
}
fn VP_Release(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    let n = r32(o + ps()).saturating_sub(1);
    w32(o + ps(), n);
    if n == 0 {
        let svc = rp(o + vp_fields().0);
        release(c, svc);
        mem::heap_free(o);
    }
    n
}
fn VP_GetVideoProcessorService(c: &mut Ctx) -> u32 {
    let (o, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let svc = rp(o + vp_fields().0);
    vs_ref(svc, 1);
    putp(out, svc);
    0
}
/// GetCreationParameters(pDeviceGuid, pVideoDesc, pRenderTargetFormat, pMaxNumSubStreams)
fn VP_GetCreationParameters(c: &mut Ctx) -> u32 {
    let (o, g, d, rt, mx) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if g == 0 && d == 0 && rt == 0 && mx == 0 {
        return E_INVALIDARG;
    }
    let (_, fg, fd, frt, fmx) = vp_fields();
    if g != 0 {
        put_guid(g, &guid(o + fg));
    }
    if d != 0 {
        mem::write(d, &mem::slice(o + fd, 36).to_vec());
    }
    if rt != 0 {
        put32(rt, r32(o + frt));
    }
    if mx != 0 {
        put32(mx, r32(o + fmx));
    }
    0
}
fn VP_GetVideoProcessorCaps(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    software_caps(c.a(1));
    0
}
/// VideoProcessBlt(pRenderTarget, pBltParams, pSamples, NumSamples, pHandleComplete): each
/// sample's SrcRect of its surface stretched (linear) to its DstRect of the target, in order,
/// by the device's StretchRect
fn VP_VideoProcessBlt(c: &mut Ctx) -> u32 {
    let (o, rt, params, samples, n) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if rt == 0 || params == 0 || (n != 0 && samples == 0) {
        return E_POINTER;
    }
    let max = r32(o + vp_fields().4);
    if n > max + 1 {
        return E_INVALIDARG;
    }
    let dev = svc_device(rp(o + vp_fields().0));
    if dev == 0 {
        return E_FAIL;
    }
    let stretch = rp(rp(dev) + 34 * ps()); // IDirect3DDevice9::StretchRect
    // DXVA2_VideoSample: Start, End (8 each), SampleFormat, SrcSurface, SrcRect, DstRect, ...
    let (src_at, rect_at, size) = if mem::x64() { (24, 32, 136) } else { (20, 24, 128) };
    for i in 0..n {
        let s = samples + size * i;
        let surf = rp(s + src_at);
        if surf == 0 {
            return E_INVALIDARG;
        }
        let hr = c.call(stretch, &[dev, surf, s + rect_at, rt, s + rect_at + 16, 2]);
        if hr & 0x8000_0000 != 0 {
            return hr;
        }
    }
    0
}

/// DXVA2CreateDirect3DDeviceManager9(pResetToken, ppDeviceManager)
fn DXVA2CreateDirect3DDeviceManager9(c: &mut Ctx) -> u32 {
    let (tok, out) = (c.a(0), c.a(1));
    if tok == 0 || out == 0 {
        return E_POINTER;
    }
    static VT: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *VT.get_or_init(|| {
        let m: [(crate::api::Handler, u32); 10] = [
            (DM_QueryInterface, 12),
            (DM_AddRef, 4),
            (DM_Release, 4),
            (DM_ResetDevice, 12),
            (DM_OpenDeviceHandle, 8),
            (DM_CloseDeviceHandle, 8),
            (DM_TestDevice, 8),
            (DM_LockDevice, 16),
            (DM_UnlockDevice, 12),
            (DM_GetVideoService, 16),
        ];
        crate::api::vtable("IDirect3DDeviceManager9", &m)
    });
    let o = mem::heap_alloc(2 * ps(), true);
    wp(o, vt);
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x1000);
    let token = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    MANAGERS.lock().unwrap().push(Manager { obj: o, refs: 1, token, ..Default::default() });
    put32(tok, token);
    putp(out, o);
    0
}

/// PHYSICAL_MONITOR { HANDLE, WCHAR szPhysicalMonitorDescription[128] }
fn GetPhysicalMonitors(c: &mut Ctx) -> u32 {
    let (n, arr) = (c.a(1), c.a(2));
    if n == 0 || arr == 0 {
        last_error(87);
        return 0;
    }
    wp(arr, 0);
    let d: Vec<u16> = "Generic Non-PnP Monitor".encode_utf16().collect();
    mem::put_wstr(arr + ps(), 128, &d);
    1
}
fn GetNumberOfPhysicalMonitors(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        last_error(87);
        return 0;
    }
    put32(c.a(1), 1);
    1
}
/// the DDC/CI calls: the monitor answers nothing
fn no_ddc(_: &mut Ctx) -> u32 {
    last_error(0xc026_2582); // ERROR_GRAPHICS_DDCCI_INVALID_DATA
    0
}

// ---------------------------------------------------------------------------------------------
// msdmo

struct Dmo {
    clsid: [u8; 16],
    name: Vec<u16>,
    category: [u8; 16],
    inputs: Vec<[u8; 32]>,
    outputs: Vec<[u8; 32]>,
}
static DMOS: Mutex<Vec<Dmo>> = Mutex::new(Vec::new());

fn read_types(n: u32, p: u32) -> Vec<[u8; 32]> {
    if p == 0 {
        return Vec::new();
    }
    (0..n.min(1024))
        .map(|i| {
            let mut t = [0u8; 32];
            t.copy_from_slice(mem::slice(p + 32 * i, 32));
            t
        })
        .collect()
}

/// a requested DMO_PARTIAL_MEDIATYPE matches a registered one (GUID_NULL subtype: any)
pub fn type_matches(want: &[u8; 32], have: &[u8; 32]) -> bool {
    want[..16] == have[..16] && (want[16..] == [0; 16] || want[16..] == have[16..])
}

/// DMORegister(name, clsid, category, flags, cIn, pIn, cOut, pOut)
fn DMORegister(c: &mut Ctx) -> u32 {
    let (np, clsid, cat) = (c.a(0), c.a(1), c.a(2));
    if np == 0 || clsid == 0 || cat == 0 || guid(cat) == [0; 16] {
        return E_INVALIDARG;
    }
    let d = Dmo { clsid: guid(clsid), name: wstr(np).into_iter().take(79).collect(), category: guid(cat), inputs: read_types(c.a(4), c.a(5)), outputs: read_types(c.a(6), c.a(7)) };
    let mut g = DMOS.lock().unwrap();
    g.retain(|x| !(x.clsid == d.clsid && x.category == d.category));
    g.push(d);
    0
}
/// DMOUnregister(clsid, category): S_FALSE when nothing was registered (GUID_NULL: every category)
fn DMOUnregister(c: &mut Ctx) -> u32 {
    let (clsid, cat) = (guid(c.a(0)), if c.a(1) == 0 { [0; 16] } else { guid(c.a(1)) });
    let mut g = DMOS.lock().unwrap();
    let n = g.len();
    g.retain(|x| !(x.clsid == clsid && (cat == [0; 16] || x.category == cat)));
    if g.len() == n { S_FALSE } else { 0 }
}
/// DMOGetName(clsid, WCHAR name[80])
fn DMOGetName(c: &mut Ctx) -> u32 {
    let (clsid, out) = (guid(c.a(0)), c.a(1));
    let g = DMOS.lock().unwrap();
    let Some(d) = g.iter().find(|x| x.clsid == clsid) else { return E_FAIL };
    if out == 0 {
        return E_POINTER;
    }
    mem::put_wstr(out, 80, &d.name);
    0
}
/// DMOGetTypes(clsid, nIn, pnIn, pIn, nOut, pnOut, pOut)
fn DMOGetTypes(c: &mut Ctx) -> u32 {
    let clsid = guid(c.a(0));
    let g = DMOS.lock().unwrap();
    let Some(d) = g.iter().find(|x| x.clsid == clsid) else { return E_FAIL };
    for (n, pn, p, list) in [(c.a(1), c.a(2), c.a(3), &d.inputs), (c.a(4), c.a(5), c.a(6), &d.outputs)] {
        let k = (n as usize).min(list.len());
        if p != 0 {
            for (i, t) in list.iter().take(k).enumerate() {
                mem::write(p + 32 * i as u32, t);
            }
        }
        if pn != 0 {
            put32(pn, if p != 0 { k as u32 } else { 0 });
        }
    }
    0
}

/// IEnumDMO over (clsid, name) results: vtable, refs, position, count, then the entries
fn enum_dmo(items: &[([u8; 16], Vec<u16>)], pos: usize) -> u32 {
    static VT: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *VT.get_or_init(|| {
        let m: [(crate::api::Handler, u32); 7] = [(ED_QueryInterface, 12), (ED_AddRef, 4), (ED_Release, 4), (ED_Next, 20), (ED_Skip, 8), (ED_Reset, 4), (ED_Clone, 8)];
        crate::api::vtable("IEnumDMO", &m)
    });
    let k = ps();
    let o = mem::heap_alloc(k + 12 + items.len() as u32 * (16 + k), true);
    wp(o, vt);
    w32(o + k, 1);
    w32(o + k + 4, pos as u32);
    w32(o + k + 8, items.len() as u32);
    for (i, (g, n)) in items.iter().enumerate() {
        let e = o + k + 12 + i as u32 * (16 + k);
        put_guid(e, g);
        let s = mem::heap_alloc(2 * n.len() as u32 + 2, true);
        mem::put_wstr(s, n.len() as u32 + 1, n);
        wp(e + 16, s);
    }
    o
}
fn ed_entry(o: u32, i: u32) -> u32 {
    o + ps() + 12 + i * (16 + ps())
}
fn ED_QueryInterface(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    // IUnknown, IEnumDMO {2c3cd98a-2bfa-4a53-9c27-5249ba64ba0f}
    if matches!(r32(iid), 0 | 0x2c3c_d98a) {
        ED_AddRef(c);
        putp(out, this);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn ED_AddRef(c: &mut Ctx) -> u32 {
    let r = c.a(0) + ps();
    w32(r, r32(r) + 1);
    r32(r)
}
fn ED_Release(c: &mut Ctx) -> u32 {
    let (this, r) = (c.a(0), c.a(0) + ps());
    let n = r32(r).saturating_sub(1);
    w32(r, n);
    if n == 0 {
        for i in 0..r32(this + ps() + 8) {
            mem::heap_free(rp(ed_entry(this, i) + 16));
        }
        mem::heap_free(this);
    }
    n
}
/// Next(cItems, pCLSID, Names, pcFetched): names are CoTaskMemAlloc'd copies
fn ED_Next(c: &mut Ctx) -> u32 {
    let (this, n, clsids, names, fetched) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if clsids == 0 {
        return E_POINTER;
    }
    if n > 1 && fetched == 0 {
        return E_INVALIDARG;
    }
    let (pos, count) = (r32(this + ps() + 4), r32(this + ps() + 8));
    let mut got = 0;
    while got < n && pos + got < count {
        let e = ed_entry(this, pos + got);
        mem::write(clsids + 16 * got, mem::slice(e, 16));
        if names != 0 {
            let s = wstr(rp(e + 16));
            let p = mem::heap_alloc(2 * s.len() as u32 + 2, true);
            if p == 0 {
                return E_OUTOFMEMORY;
            }
            mem::put_wstr(p, s.len() as u32 + 1, &s);
            wp(names + ps() * got, p);
        }
        got += 1;
    }
    w32(this + ps() + 4, pos + got);
    if fetched != 0 {
        put32(fetched, got);
    }
    if got == n { 0 } else { S_FALSE }
}
fn ED_Skip(c: &mut Ctx) -> u32 {
    let (this, n) = (c.a(0), c.a(1));
    let (pos, count) = (r32(this + ps() + 4), r32(this + ps() + 8));
    let np = (pos + n).min(count);
    w32(this + ps() + 4, np);
    if np - pos == n { 0 } else { S_FALSE }
}
fn ED_Reset(c: &mut Ctx) -> u32 {
    w32(c.a(0) + ps() + 4, 0);
    0
}
fn ED_Clone(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let items: Vec<([u8; 16], Vec<u16>)> = (0..r32(this + ps() + 8)).map(|i| (guid(ed_entry(this, i)), wstr(rp(ed_entry(this, i) + 16)))).collect();
    putp(out, enum_dmo(&items, r32(this + ps() + 4) as usize));
    0
}

/// DMOEnum(category, flags, cIn, pIn, cOut, pOut, ppEnum): the DMOs of the category (GUID_NULL:
/// all) that take every requested input type and give every requested output type
fn DMOEnum(c: &mut Ctx) -> u32 {
    let cat = if c.a(0) == 0 { [0; 16] } else { guid(c.a(0)) };
    let (ins, outs, out) = (read_types(c.a(2), c.a(3)), read_types(c.a(4), c.a(5)), c.a(6));
    if out == 0 {
        return E_POINTER;
    }
    let g = DMOS.lock().unwrap();
    let mut items: Vec<([u8; 16], Vec<u16>)> = Vec::new();
    for d in g.iter() {
        if cat != [0; 16] && d.category != cat {
            continue;
        }
        let ok = ins.iter().all(|w| d.inputs.iter().any(|h| type_matches(w, h))) && outs.iter().all(|w| d.outputs.iter().any(|h| type_matches(w, h)));
        if ok && !items.iter().any(|(c, _)| *c == d.clsid) {
            items.push((d.clsid, d.name.clone()));
        }
    }
    drop(g);
    putp(out, enum_dmo(&items, 0));
    0
}

// DMO_MEDIA_TYPE (AM_MEDIA_TYPE): majortype, subtype, bFixedSizeSamples, bTemporalCompression,
// lSampleSize, formattype, pUnk, cbFormat, pbFormat (x64: pUnk at 64, cbFormat 72, pbFormat 80)
fn mt_off() -> (u32, u32, u32, u32) {
    if mem::x64() { (64, 72, 80, 88) } else { (60, 64, 68, 72) }
}
fn MoInitMediaType(c: &mut Ctx) -> u32 {
    let (p, cb) = (c.a(0), c.a(1));
    if p == 0 {
        return E_POINTER;
    }
    let (_, cbf, pbf, size) = mt_off();
    mem::write(p, &vec![0u8; size as usize]);
    if cb != 0 {
        let f = mem::heap_alloc(cb, false);
        if f == 0 {
            return E_OUTOFMEMORY;
        }
        wp(p + pbf, f);
    }
    w32(p + cbf, cb);
    0
}
fn free_mt(c: &mut Ctx, p: u32) {
    let (unk, cbf, pbf, _) = mt_off();
    if rp(p + pbf) != 0 {
        mem::heap_free(rp(p + pbf));
        wp(p + pbf, 0);
    }
    w32(p + cbf, 0);
    let u = rp(p + unk);
    if u != 0 {
        wp(p + unk, 0);
        release(c, u);
    }
}
fn MoFreeMediaType(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if p == 0 {
        return E_POINTER;
    }
    free_mt(c, p);
    0
}
fn copy_mt(c: &mut Ctx, dst: u32, src: u32) -> u32 {
    let (unk, cbf, pbf, size) = mt_off();
    let b = mem::slice(src, size).to_vec();
    mem::write(dst, &b);
    let n = r32(src + cbf);
    if n != 0 && rp(src + pbf) != 0 {
        let f = mem::heap_alloc(n, false);
        if f == 0 {
            return E_OUTOFMEMORY;
        }
        let d = mem::slice(rp(src + pbf), n).to_vec();
        mem::write(f, &d);
        wp(dst + pbf, f);
    } else {
        wp(dst + pbf, 0);
    }
    if rp(src + unk) != 0 {
        add_ref(c, rp(src + unk));
    }
    0
}
fn MoCopyMediaType(c: &mut Ctx) -> u32 {
    let (d, s) = (c.a(0), c.a(1));
    if d == 0 || s == 0 {
        return E_POINTER;
    }
    copy_mt(c, d, s)
}
fn MoCreateMediaType(c: &mut Ctx) -> u32 {
    let (pp, cb) = (c.a(0), c.a(1));
    if pp == 0 {
        return E_POINTER;
    }
    let p = mem::heap_alloc(mt_off().3, true);
    if p == 0 {
        return E_OUTOFMEMORY;
    }
    let (_, cbf, pbf, _) = mt_off();
    if cb != 0 {
        let f = mem::heap_alloc(cb, false);
        wp(p + pbf, f);
    }
    w32(p + cbf, cb);
    putp(pp, p);
    0
}
fn MoDeleteMediaType(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if p == 0 {
        return E_POINTER;
    }
    free_mt(c, p);
    mem::heap_free(p);
    0
}
fn MoDuplicateMediaType(c: &mut Ctx) -> u32 {
    let (pp, s) = (c.a(0), c.a(1));
    if pp == 0 || s == 0 {
        return E_POINTER;
    }
    let p = mem::heap_alloc(mt_off().3, true);
    let r = copy_mt(c, p, s);
    if r != 0 {
        mem::heap_free(p);
        return r;
    }
    putp(pp, p);
    0
}

// ---------------------------------------------------------------------------------------------
// windowscodecs: the exports beside the COM classes

/// The vtable slot and argument count (with `this`) of `iface::method` (com_methods.rs, from
/// the Win32 metadata)
fn slot(iface: &str, method: &str) -> Option<(u32, u32)> {
    let (_, ms, _) = crate::com::methods::IFACES.iter().find(|(n, _, _)| *n == iface)?;
    ms.iter().position(|(m, _, _)| *m == method).map(|i| (i as u32, ms[i].1 as u32))
}
fn call_method(c: &mut Ctx, obj: u32, iface: &str, method: &str, args: &[u32]) -> u32 {
    let Some((i, _)) = slot(iface, method) else { return E_FAIL };
    let f = rp(rp(obj) + ps() * i);
    let mut a = vec![obj];
    a.extend_from_slice(args);
    c.call(f, &a)
}

/// IWICxxx_Method_Proxy(this, ...): the method through the object's vtable (the proxies the
/// DLL exports for C callers)
fn wic_proxy(c: &mut Ctx) -> u32 {
    // (what each proxy calls, found once per export: (vtable slot, argument count))
    static RESOLVED: Mutex<Option<std::collections::HashMap<u32, (u32, u32)>>> = Mutex::new(None);
    let hit = RESOLVED.lock().unwrap().as_ref().and_then(|m| m.get(&c.trap).copied());
    if let Some((i, n)) = hit {
        let this = c.a(0);
        if this == 0 {
            return E_INVALIDARG;
        }
        let mut a: Vec<u32> = (0..n).map(|k| c.a(k)).collect();
        a[0] = this;
        let f = rp(rp(this) + ps() * i);
        return c.call(f, &a);
    }
    let Some(full) = crate::api::name_of(c.trap) else { return E_FAIL };
    let name = full.split('!').nth(1).unwrap_or("");
    let Some(stem) = name.strip_suffix("_Proxy") else { return E_FAIL };
    match stem {
        // WICCreateImagingFactory_Proxy(SDKVersion, ppFactory)
        "WICCreateImagingFactory" => {
            if c.a(1) == 0 {
                return E_INVALIDARG;
            }
            putp(c.a(1), crate::wic::factory());
            return 0;
        }
        // WICCreateColorContext_Proxy(factory, ppContext)
        "WICCreateColorContext" => return call_method(c, c.a(0), "IWICImagingFactory", "CreateColorContext", &[c.a(1)]),
        _ => {}
    }
    let stem = if stem == "IWICBitmapLock_GetDataPointer_STA" { "IWICBitmapLock_GetDataPointer" } else { stem };
    let Some((iface, method)) = stem.split_once('_') else { return E_FAIL };
    let Some((_, n)) = slot(iface, method) else {
        eprintln!("sake: {name}: no such method");
        return E_FAIL;
    };
    let this = c.a(0);
    if this == 0 {
        return E_INVALIDARG;
    }
    if let Some((i, _)) = slot(iface, method) {
        RESOLVED.lock().unwrap().get_or_insert_with(Default::default).insert(c.trap, (i, n.max(1)));
    }
    let args: Vec<u32> = (1..n).map(|i| c.a(i)).collect();
    call_method(c, this, iface, method, &args)
}

/// WICConvertBitmapSource(dstFormat, pISrc, ppIDst): a format converter of the source
/// (the source itself, AddRef'd, when it is already in that format)
fn WICConvertBitmapSource(c: &mut Ctx) -> u32 {
    let (fmt, src, out) = (c.a(0), c.a(1), c.a(2));
    if fmt == 0 || src == 0 || out == 0 {
        return E_INVALIDARG;
    }
    putp(out, 0);
    let cur = mem::heap_alloc(16, true);
    let hr = call_method(c, src, "IWICBitmapSource", "GetPixelFormat", &[cur]);
    let same = hr == 0 && guid(cur) == guid(fmt);
    mem::heap_free(cur);
    if same {
        add_ref(c, src);
        putp(out, src);
        return 0;
    }
    let factory = crate::wic::factory();
    let pc = mem::heap_alloc(8, true);
    let hr = call_method(c, factory, "IWICImagingFactory", "CreateFormatConverter", &[pc]);
    let conv = rp(pc);
    mem::heap_free(pc);
    release(c, factory);
    if hr != 0 || conv == 0 {
        return if hr != 0 { hr } else { E_FAIL };
    }
    // Initialize(source, dstFormat, WICBitmapDitherTypeNone, NULL palette, 0.0 alpha threshold,
    // WICBitmapPaletteTypeCustom); the double is two zero words on x86, one zero slot on x64
    let args: Vec<u32> = if mem::x64() { vec![src, fmt, 0, 0, 0, 0] } else { vec![src, fmt, 0, 0, 0, 0, 0] };
    let hr = call_method(c, conv, "IWICFormatConverter", "Initialize", &args);
    if hr != 0 {
        release(c, conv);
        return hr;
    }
    putp(out, conv);
    0
}

/// The registered ETW providers (REGHANDLEs, 64-bit on x86 too)
static ETW: Mutex<Vec<u64>> = Mutex::new(Vec::new());
static ETW_NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A 64-bit argument that starts at x86 stack slot `slot` (x64: argument `arg`)
fn arg64(c: &Ctx, slot: u32, arg: u32) -> u64 {
    if mem::x64() { c.a64(arg) } else { c.a(slot) as u64 | (c.a(slot + 1) as u64) << 32 }
}

/// EventRegister(guid, callback, context, &handle)
fn etw_register(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 || c.a(3) == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let h = ETW_NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    ETW.lock().unwrap().push(h);
    mem::w64(c.a(3), h);
    0
}
/// EventUnregister(handle)
fn etw_unregister(c: &mut Ctx) -> u32 {
    let h = arg64(c, 0, 0);
    let mut g = ETW.lock().unwrap();
    match g.iter().position(|&x| x == h) {
        Some(i) => {
            g.remove(i);
            0
        }
        None => 6, // ERROR_INVALID_HANDLE
    }
}
/// EventWriteString(handle, level, keyword, text)
fn etw_write_string(c: &mut Ctx) -> u32 {
    let (h, text) = (arg64(c, 0, 0), if mem::x64() { c.a(3) } else { c.a(5) });
    if h == 0 {
        return 6;
    }
    if text == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    0
}

pub fn init() {
    for d in ["advapi32", "ntdll"] {
        let n = |s: &str| if d == "ntdll" { format!("Etw{s}") } else { s.to_string() };
        implement(d, &n("EventRegister"), etw_register);
        implement(d, &n("EventUnregister"), etw_unregister);
        implement(d, &n("EventWriteString"), etw_write_string);
        // nobody listens: written events go nowhere, no provider is enabled
        for f in ["EventWrite", "EventWriteEx", "EventWriteTransfer", "EventSetInformation", "EventActivityIdControl", "EventEnabled", "EventProviderEnabled"] {
            implement(d, &n(f), |_| 0);
        }
    }
    // dnsapi
    implement("dnsapi", "DnsValidateName_W", |c| if c.a(0) == 0 { ERROR_INVALID_NAME } else { validate_name(&wstr(c.a(0)), c.a(1)) });
    for n in ["DnsValidateName_A", "DnsValidateName_UTF8", "DnsValidateName"] {
        implement("dnsapi", n, |c| {
            if c.a(0) == 0 {
                return ERROR_INVALID_NAME;
            }
            let w: Vec<u16> = String::from_utf8_lossy(&cstr(c.a(0))).encode_utf16().collect();
            validate_name(&w, c.a(1))
        });
    }
    implement("dnsapi", "DnsNameCompare_W", |c| {
        let s = |p: u32| (p != 0).then(|| wstr(p));
        names_equal(s(c.a(0)).as_deref(), s(c.a(1)).as_deref()) as u32
    });
    implement("dnsapi", "DnsNameCompare_A", |c| {
        let s = |p: u32| (p != 0).then(|| String::from_utf8_lossy(&cstr(p)).encode_utf16().collect::<Vec<u16>>());
        names_equal(s(c.a(0)).as_deref(), s(c.a(1)).as_deref()) as u32
    });
    implement("dnsapi", "DnsQuery_W", |c| dns_query(c, 1));
    implement("dnsapi", "DnsQuery_UTF8", |c| dns_query(c, 2));
    implement("dnsapi", "DnsQuery_A", |c| dns_query(c, 3));
    implement("dnsapi", "DnsRecordListFree", dns_free);
    implement("dnsapi", "DnsFree", dns_free);
    implement("dnsapi", "DnsFlushResolverCache", |_| 1);
    for n in ["DnsFlushResolverCacheEntry_A", "DnsFlushResolverCacheEntry_W", "DnsFlushResolverCacheEntry_UTF8"] {
        implement("dnsapi", n, |c| (c.a(0) != 0) as u32);
    }
    implement("dnsapi", "DnsExtractRecordsFromMessage_UTF8", |c| dns_extract(c, 2));
    implement("dnsapi", "DnsExtractRecordsFromMessage_W", |c| dns_extract(c, 1));
    implement("dnsapi", "DnsRecordCopyEx", |c| if c.a(0) == 0 { 0 } else { dnsrec::copy(c.a(0), c.a(1), c.a(2)) });
    implement("dnsapi", "DnsRecordSetCopyEx", |c| set_copy(c.a(0), c.a(1), c.a(2)));
    implement("dnsapi", "DnsRecordCompare", |c| if c.a(0) == 0 || c.a(1) == 0 { 0 } else { record_compare(c.a(0), c.a(1)) as u32 });
    implement("dnsapi", "DnsRecordSetCompare", record_set_compare);
    implement("dnsapi", "DnsRecordSetDetach", record_set_detach);
    implement("dnsapi", "DnsGetCacheDataTable", cache_table);
    implement("dnsapi", "DnsServiceConstructInstance", service_construct);
    implement("dnsapi", "DnsServiceFreeInstance", service_free);
    // (no adapters with DNS servers)
    implement("dnsapi", "DnsQueryConfig", |_| 9852); // DNS_ERROR_NO_DNS_SERVERS

    // dhcpcsvc: no adapter with DHCP
    implement("dhcpcsvc", "DhcpCApiInitialize", |c| {
        if c.a(0) != 0 {
            put32(c.a(0), 2);
        }
        0
    });
    implement("dhcpcsvc", "DhcpCApiCleanup", |_| 0);
    implement("dhcpcsvc", "DhcpRequestParams", |_| 87);

    // tbs: no TPM
    for n in ["Tbsi_Context_Create", "Tbsi_GetDeviceInfo", "GetDeviceIDString"] {
        implement("tbs", n, |_| 0x8028_400f); // TBS_E_TPM_NOT_FOUND
    }

    // dxva2
    implement("dxva2", "DXVA2CreateDirect3DDeviceManager9", DXVA2CreateDirect3DDeviceManager9);
    implement("dxva2", "DXVA2CreateVideoService", |c| {
        let (dev, iid, out) = (c.a(0), c.a(1), c.a(2));
        if dev == 0 || out == 0 {
            return E_POINTER;
        }
        add_ref(c, dev);
        let o = new_service(dev, 0);
        let hr = service_qi(o, iid, out);
        let f = rp(rp(o) + 2 * ps());
        c.call(f, &[o]);
        hr
    });
    implement("dxva2", "GetNumberOfPhysicalMonitorsFromHMONITOR", GetNumberOfPhysicalMonitors);
    implement("dxva2", "GetNumberOfPhysicalMonitorsFromIDirect3DDevice9", GetNumberOfPhysicalMonitors);
    implement("dxva2", "GetPhysicalMonitorsFromHMONITOR", GetPhysicalMonitors);
    implement("dxva2", "GetPhysicalMonitorsFromIDirect3DDevice9", GetPhysicalMonitors);
    implement("dxva2", "DestroyPhysicalMonitor", |_| 1);
    implement("dxva2", "DestroyPhysicalMonitors", |_| 1);
    for n in [
        "GetMonitorBrightness", "GetMonitorCapabilities", "GetMonitorColorTemperature", "GetMonitorContrast", "GetMonitorDisplayAreaPosition",
        "GetMonitorDisplayAreaSize", "GetMonitorRedGreenOrBlueDrive", "GetMonitorRedGreenOrBlueGain", "GetMonitorTechnologyType",
        "GetVCPFeatureAndVCPFeatureReply", "GetCapabilitiesStringLength", "CapabilitiesRequestAndCapabilitiesReply", "GetTimingReport",
        "SetMonitorBrightness", "SetMonitorColorTemperature", "SetMonitorContrast", "SetMonitorDisplayAreaPosition", "SetMonitorDisplayAreaSize",
        "SetMonitorRedGreenOrBlueDrive", "SetMonitorRedGreenOrBlueGain", "SetVCPFeature", "DegaussMonitor", "RestoreMonitorFactoryColorDefaults",
        "RestoreMonitorFactoryDefaults", "SaveCurrentMonitorSettings", "SaveCurrentSettings",
    ] {
        implement("dxva2", n, no_ddc);
    }

    // ksuser: no kernel-streaming devices
    for n in ["KsCreatePin", "KsCreateAllocator", "KsCreateClock", "KsCreateTopologyNode"] {
        implement("ksuser", n, |_| 2); // ERROR_FILE_NOT_FOUND
    }

    // msdmo
    implement("msdmo", "DMORegister", DMORegister);
    implement("msdmo", "DMOUnregister", DMOUnregister);
    implement("msdmo", "DMOGetName", DMOGetName);
    implement("msdmo", "DMOGetTypes", DMOGetTypes);
    implement("msdmo", "DMOEnum", DMOEnum);
    implement("msdmo", "MoInitMediaType", MoInitMediaType);
    implement("msdmo", "MoFreeMediaType", MoFreeMediaType);
    implement("msdmo", "MoCopyMediaType", MoCopyMediaType);
    implement("msdmo", "MoCreateMediaType", MoCreateMediaType);
    implement("msdmo", "MoDeleteMediaType", MoDeleteMediaType);
    implement("msdmo", "MoDuplicateMediaType", MoDuplicateMediaType);

    // windowscodecs
    implement("windowscodecs", "WICConvertBitmapSource", WICConvertBitmapSource);
    for n in crate::api::spec_names("windowscodecs") {
        if n.ends_with("_Proxy") {
            implement("windowscodecs", &n, wic_proxy);
        }
    }
    // (no metadata schemas or short names registered; sections: not mapped as bitmaps)
    for n in ["WICMapGuidToShortName", "WICMapShortNameToGuid", "WICMapSchemaToName"] {
        implement("windowscodecs", n, |_| 0x8898_2f40); // WINCODEC_ERR_PROPERTYNOTFOUND
    }
    for n in ["WICCreateBitmapFromSection", "WICCreateBitmapFromSectionEx"] {
        implement("windowscodecs", n, |_| 0x8000_4001); // E_NOTIMPL
    }

    // hhctrl: no HTML Help window
    implement("hhctrl", "HtmlHelpA", |_| 0);
    implement("hhctrl", "HtmlHelpW", |_| 0);
    implement("hhctrl", "doWinMain", |_| 0);
    let _ = (r16, E_HANDLE);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// review b09: a wild property count is refused at once; counts up to the limit copy every
    /// key and value and free again
    #[test]
    fn service_instances_bound_their_properties() {
        let _g = crate::test_guest::enter();
        crate::test_guest::guest_thread();
        let name = mem::heap_alloc(16, true);
        mem::write(name, &[b'a', 0, b'b', 0, 0, 0]);
        let t = std::time::Instant::now();
        for n in [SERVICE_PROPS_MAX + 1, 0x4000_0000, u32::MAX] {
            assert_eq!(crate::test_guest::call(service_construct, &[name, name, 0, 0, 80, 1, 2, n, name, name]), 0);
        }
        assert!(t.elapsed().as_secs() < 2);
        let k = ps();
        for n in [0u32, 1, 7] {
            let arr = mem::heap_alloc(k * n.max(1), true);
            for i in 0..n {
                wp(arr + k * i, name);
            }
            let o = crate::test_guest::call(service_construct, &[name, name, 0, 0, 80, 1, 2, n, arr, arr]);
            assert_ne!(o, 0);
            let (_, count, keys, _) = svc_off();
            assert_eq!(r32(o + count), n);
            for i in 0..n {
                let kp = rp(rp(o + keys) + k * i);
                assert_eq!(mem::wstr(kp), w("ab"));
            }
            crate::test_guest::call(service_free, &[o]);
            mem::heap_free(arr);
        }
        mem::heap_free(name);
    }

    /// Wine's dlls/dnsapi/tests/name.c table (what Windows answers; where it lists an older
    /// Windows' "broken" answer, the current one)
    #[test]
    fn validate_name_table() {
        let a63 = "a".repeat(63);
        let long4 = [a63.as_str(); 4].join(".");
        let a64 = "a".repeat(64);
        let t: Vec<(&str, u32, u32)> = vec![
            ("", 0, ERROR_INVALID_NAME), (".", 0, 0), ("..", 0, ERROR_INVALID_NAME), (".a", 0, ERROR_INVALID_NAME), ("a.", 0, 0),
            ("a..", 0, ERROR_INVALID_NAME), (&a64, 0, ERROR_INVALID_NAME), (&long4, 0, ERROR_INVALID_NAME),
            ("a.?", 0, DNS_ERROR_INVALID_NAME_CHAR), ("a.*", 0, DNS_ERROR_INVALID_NAME_CHAR), ("a ", 0, DNS_ERROR_INVALID_NAME_CHAR),
            ("a._b", 0, DNS_ERROR_NON_RFC_NAME), ("123", 0, 0), ("123.456", 0, 0), ("a.b", 0, 0),
            ("", 1, ERROR_INVALID_NAME), (".", 1, ERROR_INVALID_NAME), ("..", 1, ERROR_INVALID_NAME), (".c", 1, ERROR_INVALID_NAME),
            ("c.", 1, ERROR_INVALID_NAME), ("c..", 1, ERROR_INVALID_NAME), (&a64, 1, ERROR_INVALID_NAME), ("?", 1, DNS_ERROR_INVALID_NAME_CHAR),
            ("*", 1, DNS_ERROR_INVALID_NAME_CHAR), ("c ", 1, DNS_ERROR_INVALID_NAME_CHAR), ("_c", 1, DNS_ERROR_NON_RFC_NAME), ("456", 1, 0),
            ("456.789", 1, ERROR_INVALID_NAME), ("c.d", 1, ERROR_INVALID_NAME),
            ("", 2, ERROR_INVALID_NAME), (".", 2, 0), ("..", 2, ERROR_INVALID_NAME), (".e", 2, ERROR_INVALID_NAME), ("e.", 2, 0),
            ("e..", 2, ERROR_INVALID_NAME), (&a64, 0, ERROR_INVALID_NAME), (&long4, 2, ERROR_INVALID_NAME), ("?", 3, DNS_ERROR_INVALID_NAME_CHAR),
            ("e.?", 2, DNS_ERROR_INVALID_NAME_CHAR), ("e.*", 2, DNS_ERROR_INVALID_NAME_CHAR), ("e ", 2, DNS_ERROR_INVALID_NAME_CHAR),
            ("e._f", 2, DNS_ERROR_NON_RFC_NAME), ("789", 2, 0), ("789.456", 2, 0), ("e.f", 2, 0),
            ("", 3, ERROR_INVALID_NAME), (".", 3, ERROR_INVALID_NAME), ("..", 3, ERROR_INVALID_NAME), (".g", 3, ERROR_INVALID_NAME),
            ("g.", 3, ERROR_INVALID_NAME), ("g..", 3, ERROR_INVALID_NAME), (&a64, 3, ERROR_INVALID_NAME), ("*", 3, DNS_ERROR_INVALID_NAME_CHAR),
            ("g ", 3, DNS_ERROR_INVALID_NAME_CHAR), ("_g", 3, DNS_ERROR_NON_RFC_NAME), ("123", 3, 0), ("123.456", 3, ERROR_INVALID_NAME),
            ("g.h", 3, ERROR_INVALID_NAME),
            ("", 4, ERROR_INVALID_NAME), (".", 4, ERROR_INVALID_NAME), ("..", 4, ERROR_INVALID_NAME), (".j", 4, ERROR_INVALID_NAME),
            ("j.", 4, ERROR_INVALID_NAME), ("j..", 4, ERROR_INVALID_NAME), (&a64, 4, ERROR_INVALID_NAME), (&long4, 4, ERROR_INVALID_NAME),
            ("?", 4, ERROR_INVALID_NAME), ("i ", 4, ERROR_INVALID_NAME), ("_i", 4, ERROR_INVALID_NAME), ("123", 4, ERROR_INVALID_NAME),
            ("123.456", 4, ERROR_INVALID_NAME), ("i.j", 4, ERROR_INVALID_NAME), ("*", 4, 0), ("*j", 4, DNS_ERROR_INVALID_NAME_CHAR),
            ("*.j", 4, 0), ("i.*", 4, ERROR_INVALID_NAME),
            ("", 5, ERROR_INVALID_NAME), (".", 5, ERROR_INVALID_NAME), ("..", 5, ERROR_INVALID_NAME), (".k", 5, ERROR_INVALID_NAME),
            ("k.", 5, ERROR_INVALID_NAME), ("k..", 5, ERROR_INVALID_NAME), (&a64, 5, ERROR_INVALID_NAME), (&long4, 5, ERROR_INVALID_NAME),
            ("?", 5, ERROR_INVALID_NAME), ("k ", 5, ERROR_INVALID_NAME), ("_k", 5, 0), ("123", 5, ERROR_INVALID_NAME),
            ("123.456", 5, ERROR_INVALID_NAME), ("k.l", 5, ERROR_INVALID_NAME), ("_", 5, DNS_ERROR_NON_RFC_NAME), ("_k.l", 5, 0),
            ("k._l", 5, ERROR_INVALID_NAME),
        ];
        let bad: Vec<String> = t
            .iter()
            .filter_map(|&(n, f, want)| {
                let got = validate_name(&w(n), f);
                (got != want).then(|| format!("{:?} fmt {f}: got {got}, want {want}", &n[..n.len().min(12)]))
            })
            .collect();
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// dlls/dnsapi/tests/name.c test_DnsNameCompare_A
    #[test]
    fn name_compare_table() {
        let eq = |a: &str, b: &str| names_equal(Some(&w(a)), Some(&w(b)));
        assert!(names_equal(None, None));
        assert!(!names_equal(Some(&w("a")), None) && !names_equal(None, Some(&w("a"))));
        for (a, b) in [("", ""), (".", ""), ("", "."), (".", ".."), ("..", "."), ("a", "a"), ("a", "A"), ("A", "a"), ("a.b", "A.B"), ("a.b", "a.b."), ("a.b..", "a.b")] {
            assert!(eq(a, b), "{a:?} {b:?}");
        }
        for (a, b) in [("a", "b"), ("a", "B"), ("A", "b"), ("a.b", "b.a"), ("a.b", "B.A"), ("a.b.", "b.a.."), ("a.a", "a.b"), ("a", "a.a")] {
            assert!(!eq(a, b), "{a:?} {b:?}");
        }
    }

    #[test]
    fn literals() {
        assert_eq!(ip_literal("192.168.111.11"), Some(vec![192, 168, 111, 11]));
        assert_eq!(ip_literal("2001:db8:3333:4444:5555:6666:7777:8888").map(|v| v.len()), Some(16));
        assert_eq!(ip_literal("winehq.org"), None);
        assert_eq!(ip_literal(" 192.168.111.11"), None);
    }

    #[test]
    fn dmo_type_wildcards() {
        let mut a = [0u8; 32];
        a[0] = 1;
        let mut b = a;
        b[16] = 7;
        assert!(type_matches(&a, &b)); // (subtype GUID_NULL: any)
        assert!(type_matches(&b, &b));
        let mut c = b;
        c[17] = 1;
        assert!(!type_matches(&c, &b));
        let mut d = b;
        d[0] = 2;
        assert!(!type_matches(&d, &b));
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod issue88 {
    use super::*;
    use crate::test_guest::{enter, call};
    #[test]
    fn enumerator_advances_and_reports_partial_fetches() {
        let _g = enter();
        let p = mem::heap_alloc(128, true);
        w32(p + ps() + 8, 2);
        mem::write(ed_entry(p, 0), &[0x11; 16]);
        mem::write(ed_entry(p, 1), &[0x22; 16]);
        // A broken +=1 can spin forever: surface that as a bounded assertion failure.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || { tx.send(call(ED_Next, &[p, 3, p + 64, 0, p + 120])).unwrap(); });
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(2)).expect("Next must finish"), S_FALSE);
        assert_eq!(r32(p + 120), 2);
        assert_eq!(mem::slice(p + 64, 32), [&[0x11; 16][..], &[0x22; 16][..]].concat());
        assert_eq!(call(ED_Next, &[p, 1, p + 64, 0, p + 120]), S_FALSE);
        assert_eq!(r32(p + 120), 0);
        mem::heap_free(p);
    }
}
