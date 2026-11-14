//! COM local-server activation and typed calls over the shared kernel. Method
//! signatures come from the registered proxy DLL's MIDL metadata. Vendor object
//! code executes in its original server process; no method results are invented.
#![allow(non_snake_case)]
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU32, Ordering}};
use std::time::{Duration, Instant};
use crate::api::{self, Ctx};
use crate::kern::{self, com::{Call, Class, Reply}, enc::Enc};
use crate::mem::{self, ps, r16, r32, rp, w32, wp};
use crate::ndr::{self, Kind, Method, Param};

const NOTIMPL: u32 = 0x8000_4001;
const NOINTERFACE: u32 = 0x8000_4002;
const POINTER: u32 = 0x8000_4003;
const BAD_DATA: u32 = 0x8007_06f7;
const DISCONNECTED: u32 = 0x8001_0108;
const CLASSNOTREG: u32 = 0x8004_0154;
const UNKNOWN: [u8; 16] = [0,0,0,0,0,0,0,0,0xc0,0,0,0,0,0,0,0x46];
fn guid(p: u32) -> Vec<u8> { if p == 0 { vec![] } else { mem::slice(p, 16).to_vec() } }
pub(crate) fn text(g: &[u8]) -> String {
    if g.len() != 16 { return String::new(); }
    format!("{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
        u32::from_le_bytes(g[..4].try_into().unwrap()), u16::from_le_bytes(g[4..6].try_into().unwrap()),
        u16::from_le_bytes(g[6..8].try_into().unwrap()), g[8],g[9],g[10],g[11],g[12],g[13],g[14],g[15])
}
fn qi(cpu: &mut koji::Cpu, object: u32, iid: &[u8]) -> Result<u32, u32> {
    let temp = mem::heap_alloc(16 + ps(), true); mem::write(temp, iid);
    let hr = crate::call(cpu, rp(rp(object)), &[object, temp, temp + 16]);
    let out = rp(temp + 16); mem::heap_free(temp);
    if hr == 0 && out != 0 { Ok(out) } else { Err(if hr == 0 { NOINTERFACE } else { hr }) }
}
fn refcall(cpu: &mut koji::Cpu, object: u32, method: u32) -> u32 {
    crate::call(cpu, rp(rp(object) + method * ps()), &[object])
}
fn bounded_bytes(p: u32) -> Option<Vec<u8>> {
    let base = crate::pe::containing(p)?;
    let n = crate::pe::with(base, |m| m.size.checked_sub(p - base))??;
    Some(mem::slice(p, n.min(65536)).to_vec())
}
#[derive(Clone)]
struct Interface { iid: Vec<u8>, count: u32, methods: BTreeMap<u16, Method>, abi: BTreeMap<u16, u32> }
static INTERFACES: Mutex<Vec<Arc<Interface>>> = Mutex::new(Vec::new());
fn target_name(mut p: u32) -> Option<String> {
    for _ in 0..8 {
        if let Some(name) = api::name_of(p) { return Some(name); }
        if mem::r8(p) == 0xe9 { p = p.wrapping_add(5).wrapping_add(r32(p + 1)); }
        else if mem::r8(p) == 0xff && mem::r8(p + 1) == 0x25 {
            p = rp(if mem::x64() { p.wrapping_add(6).wrapping_add(r32(p + 2)) } else { r32(p + 2) });
        } else { return None; }
    }
    None
}
fn read_interfaces(list: u32) -> Vec<Arc<Interface>> {
    let mut result = Vec::new();
    for lib in 0..64 {
        let info = rp(list + lib * ps()); if info == 0 { break; }
        let table = rp(info + ps());
        let count = r16(info + 5 * ps()) as u32;
        if count > 1024 { break; }
        for i in 0..count {
            let stub = rp(table + i * ps()); if stub == 0 { continue; }
            let iid = guid(rp(stub));
            let server = rp(stub + ps());
            let count = r32(stub + 2 * ps());
            if server == 0 || count < 3 || count > 256 { continue; }
            let desc = rp(server);
            let procs = rp(server + 2 * ps());
            let offsets = rp(server + 3 * ps());
            let Some(types) = bounded_bytes(rp(desc + 8 * ps())) else { continue; };
            let user_table = rp(desc + if mem::x64() { 13 * ps() } else { 14 * ps() });
            let mut methods = BTreeMap::new();
            let mut abi = BTreeMap::new();
            for slot in 3..count {
                let proc = procs + r16(offsets + 2 * slot) as u32;
                let Some(bytes) = bounded_bytes(proc) else { continue; };
                if bytes.len() >= 10 && bytes[0] == 0x33 {
                    let stack = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
                    if stack >= 2 * ps() && stack <= 4096 && stack % ps() == 0 { abi.insert(slot as u16, (stack / ps() - 1) * 4); }
                }
                if let Some(method) = ndr::parse(&bytes, &types, ps() as u16) {
                    let users_supported = method.params.iter().all(|p| match p.kind {
                        Kind::User { index, size } => size as u32 == ps() && user_table != 0 && index < 128 &&
                            target_name(rp(user_table + index as u32 * 4 * ps())).is_some_and(|n| n.ends_with("!BSTR_UserSize")),
                        _ => true,
                    });
                    if users_supported && method.number as u32 == slot { methods.insert(slot as u16, method); }
                }
            }
            if abi.len() as u32 == count - 3 { result.push(Arc::new(Interface { iid, count, methods, abi })); }
        }
    }
    result
}
fn interface(cpu: &mut koji::Cpu, iid: &[u8]) -> Result<Arc<Interface>, u32> {
    if let Some(i) = INTERFACES.lock().unwrap().iter().find(|i| i.iid == iid).cloned() { return Ok(i); }
    if iid == UNKNOWN { return Ok(Arc::new(Interface { iid: iid.to_vec(), count: 3, methods: BTreeMap::new(), abi: BTreeMap::new() })); }
    if MISSES.lock().unwrap().contains(iid) { return Err(NOINTERFACE); }
    let proxy_class = crate::misc::registry_string(&format!("hkcr\\interface\\{}\\proxystubclsid32", text(iid)), "").ok_or(NOINTERFACE)?;
    let dll = crate::misc::registry_string(&format!("hkcr\\clsid\\{proxy_class}\\inprocserver32"), "").ok_or(NOINTERFACE)?;
    let base = crate::pe::load_library(cpu, &dll);
    let entry = crate::pe::proc_address(base, crate::pe::Import::Name("GetProxyDllInfo")).ok_or(NOTIMPL)?;
    let out = mem::heap_alloc(2 * ps(), true);
    crate::call(cpu, entry, &[out, out + ps()]);
    let list = rp(out); mem::heap_free(out);
    if list == 0 { return Err(NOINTERFACE); }
    let all = read_interfaces(list);
    let found = all.iter().find(|i| i.iid == iid).cloned();
    add_interfaces(all);
    if found.is_none() {
        MISSES.lock().unwrap().insert(iid.to_vec());
    }
    found.ok_or(NOINTERFACE)
}
/// IIDs whose proxy DLL was loaded and did not have them (asked again, the DLL is not loaded
/// and parsed again)
static MISSES: Mutex<BTreeSet<Vec<u8>>> = Mutex::new(BTreeSet::new());
/// Adds interfaces not known yet (the first registration of an IID stays, as lookups find it
/// first; the list does not grow by the DLL's whole list per load)
fn add_interfaces(new: Vec<Arc<Interface>>) {
    let mut l = INTERFACES.lock().unwrap();
    let mut misses = MISSES.lock().unwrap();
    for i in new {
        misses.remove(&i.iid);
        if !l.iter().any(|o| o.iid == i.iid) {
            l.push(i);
        }
    }
}
type Values = Vec<(bool, Vec<u8>)>;
fn values(bytes: &[u8]) -> Result<Values, u32> {
    if bytes.len() > 16 * 1024 * 1024 { return Err(BAD_DATA); }
    let mut reader = bytes;
    let v = Values::get(&mut reader).ok_or(BAD_DATA)?;
    if !reader.is_empty() { return Err(BAD_DATA); }
    Ok(v)
}
fn encoded(values: &Values) -> Vec<u8> { let mut b = vec![]; values.put(&mut b); b }
fn size(p: &Param, args: &[u64]) -> Result<u32, u32> {
    let n = match p.kind {
        Kind::Scalar(n) | Kind::Fixed(n) => n,
        Kind::User { .. } => ps(),
        Kind::Array { element, count_stack } => {
            let count = *args.get(count_stack as usize / ps() as usize).ok_or(BAD_DATA)?;
            if count > 8 * 1024 * 1024 { return Err(BAD_DATA); }
            (count as u32).checked_mul(element as u32).ok_or(BAD_DATA)?
        }
    };
    if n > 8 * 1024 * 1024 { return Err(BAD_DATA); }
    Ok(n)
}
fn bstr(bytes: &[u8]) -> u32 {
    let p = mem::heap_alloc(bytes.len() as u32 + 6, true);
    w32(p, bytes.len() as u32); mem::write(p + 4, bytes); p + 4
}
fn read_value(p: &Param, arg: u64, args: &[u64], input: bool) -> Result<(bool, Vec<u8>), u32> {
    let address = arg as u32;
    if p.reference && address == 0 { return if p.nullable { Ok((false, vec![])) } else { Err(POINTER) }; }
    if input && !p.input() || !input && !p.output() { return Ok((true, vec![])); }
    if matches!(p.kind, Kind::User { .. }) {
        let p = if p.reference { rp(address) } else { address };
        if p == 0 { return Ok((false, vec![])); }
        let n = r32(p - 4); if n > 8 * 1024 * 1024 { return Err(BAD_DATA); }
        return Ok((true, mem::slice(p, n).to_vec()));
    }
    let n = size(p, args)?;
    if p.reference { Ok((true, mem::slice(address, n).to_vec())) }
    else if matches!(p.kind, Kind::Scalar(_)) { Ok((true, arg.to_le_bytes()[..n as usize].to_vec())) }
    else { Err(NOTIMPL) }
}
fn call_remote(cpu: &mut koji::Cpu, call: Call) -> Reply {
    let target = call.to;
    let id = kern::with(|k| { let id = k.com.submit(call); k.changed(); id }).0;
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let (reply, generation) = kern::with(|k| {
            if let Some(reply) = k.com.replies.remove(&id) { k.changed(); return Some(reply); }
            if k.exit_code(target).is_some() { return Some(Reply { hr: DISCONNECTED, data: vec![] }); }
            None
        });
        if let Some(reply) = reply { return reply; }
        pump(cpu, true);
        if !kern::wait_change(generation, Some(until)) {
            kern::with(|k| { k.com.abandon(id); k.changed(); });
            return Reply { hr: 0x8001_011f, data: vec![] };
        }
    }
}

#[derive(Clone)]
struct Export { owner: u32, pointer: u32, tid: u32, iid: Vec<u8>, refs: u32 }
static EXPORTS: Mutex<Option<HashMap<u32, Export>>> = Mutex::new(None);
static NEXT_EXPORT: AtomicU32 = AtomicU32::new(1);
static FACTORIES: Mutex<Option<HashMap<u32, (u32, u32)>>> = Mutex::new(None);
static NEXT_COOKIE: AtomicU32 = AtomicU32::new(1);
static REGISTERED: AtomicBool = AtomicBool::new(false);
fn export(pointer: u32, owner: u32, iid: Vec<u8>) -> u32 {
    let id = NEXT_EXPORT.fetch_add(1, Ordering::Relaxed);
    EXPORTS.lock().unwrap().get_or_insert_with(HashMap::new).insert(id,
        Export { owner, pointer, tid: crate::cur().tid, iid, refs: 1 });
    id
}
fn invoke(cpu: &mut koji::Cpu, object: &Export, method: &Method, bytes: &[u8]) -> Reply {
    let values = match values(bytes) { Ok(v) => v, Err(hr) => return Reply { hr, data: vec![] } };
    let params: Vec<&Param> = method.params.iter().filter(|p| !p.retval()).collect();
    if params.len() != values.len() { return Reply { hr: BAD_DATA, data: vec![] }; }
    if params.iter().any(|p| matches!(p.kind, Kind::User { .. }) && p.input() && p.output()) {
        return Reply { hr: NOTIMPL, data: vec![] };
    }
    let mut args = vec![0u64; method.stack as usize / ps() as usize - 1];
    args[0] = object.pointer as u64;
    // Correlated array lengths can refer to any scalar input argument.
    for (p, (_, bytes)) in params.iter().zip(&values) {
        if let Kind::Scalar(n) = p.kind {
            if !p.reference && p.input() {
                if bytes.len() != n as usize { return Reply { hr: BAD_DATA, data: vec![] }; }
                let mut b = [0u8; 8]; b[..bytes.len()].copy_from_slice(bytes);
                let at = p.stack as usize / ps() as usize;
                if at >= args.len() { return Reply { hr: BAD_DATA, data: vec![] }; }
                args[at] = u64::from_le_bytes(b);
                if ps() == 4 && n == 8 {
                    if at + 1 >= args.len() { return Reply { hr: BAD_DATA, data: vec![] }; }
                    args[at + 1] = args[at] >> 32;
                }
            }
        }
    }
    let mut allocations = Vec::new();
    let mut strings = Vec::new();
    let result = (|| -> Result<Reply, u32> {
        for (p, (present, bytes)) in params.iter().zip(&values) {
            let at = p.stack as usize / ps() as usize;
            if at >= args.len() { return Err(BAD_DATA); }
            if !p.input() && !bytes.is_empty() { return Err(BAD_DATA); }
            if matches!(p.kind, Kind::User { .. }) {
                if bytes.len() > 8 * 1024 * 1024 { return Err(BAD_DATA); }
                let value = if p.input() && *present { bstr(bytes) } else { 0 };
                if value != 0 { strings.push(value); }
                if p.reference {
                    let cell = mem::heap_alloc(ps(), true); allocations.push(cell); wp(cell, value); args[at] = cell as u64;
                } else { args[at] = value as u64; }
            } else if p.reference {
                if !*present {
                    if !p.nullable { return Err(POINTER); }
                    args[at] = 0; continue;
                }
                let n = size(p, &args)?;
                if p.input() && bytes.len() != n as usize { return Err(BAD_DATA); }
                let buffer = mem::heap_alloc(n.max(1), true); allocations.push(buffer);
                if p.input() { mem::write(buffer, bytes); }
                args[at] = buffer as u64;
            } else if !matches!(p.kind, Kind::Scalar(_)) { return Err(NOTIMPL); }
        }
        let method_ptr = rp(rp(object.pointer) + method.number as u32 * ps());
        let hr = if mem::x64() { crate::call64(cpu, mem::widen(method_ptr), &args) as u32 }
            else { crate::call(cpu, method_ptr, &args.iter().map(|a| *a as u32).collect::<Vec<_>>()) };
        let mut outputs = Vec::new();
        for p in &params {
            let arg = args[p.stack as usize / ps() as usize];
            outputs.push(read_value(p, arg, &args, false)?);
            if p.reference && p.output() && matches!(p.kind, Kind::User { .. }) {
                let value = rp(arg as u32); if value != 0 { strings.push(value); }
            }
        }
        Ok(Reply { hr, data: encoded(&outputs) })
    })();
    for value in strings { crate::misc::bstr_free(value); }
    for buffer in allocations { mem::heap_free(buffer); }
    result.unwrap_or_else(|hr| Reply { hr, data: vec![] })
}
fn serve(cpu: &mut koji::Cpu, call: &Call) -> Reply {
    if call.op == 0 {
        let class = kern::with(|k| k.com.classes.get(&call.class).cloned()).0;
        let Some(class) = class.filter(|c| c.pid == crate::pid() && c.tid == crate::cur().tid && !c.suspended) else {
            return Reply { hr: CLASSNOTREG, data: vec![] };
        };
        let factory = FACTORIES.lock().unwrap().as_ref().and_then(|f| f.get(&class.cookie)).copied();
        let Some((factory, _)) = factory else { return Reply { hr: CLASSNOTREG, data: vec![] } };
        let buffer = mem::heap_alloc(16 + ps(), true); mem::write(buffer, &call.iid);
        let hr = crate::call(cpu, rp(rp(factory) + 3 * ps()), &[factory, 0, buffer, buffer + 16]);
        let pointer = rp(buffer + 16); mem::heap_free(buffer);
        if hr != 0 || pointer == 0 { return Reply { hr: if hr == 0 { NOINTERFACE } else { hr }, data: vec![] }; }
        if class.flags & 1 == 0 {
            kern::with(|k| { k.com.classes.remove(&call.class); k.changed(); });
        }
        return Reply { hr: 0, data: export(pointer, call.from, call.iid.clone()).to_le_bytes().to_vec() };
    }
    let object = EXPORTS.lock().unwrap().as_ref().and_then(|m| m.get(&call.object)).cloned();
    let Some(object) = object.filter(|o| o.owner == call.from && o.tid == crate::cur().tid) else {
        return Reply { hr: DISCONNECTED, data: vec![] };
    };
    match call.op {
        1 => {
            let info = match interface(cpu, &object.iid) { Ok(i) => i, Err(hr) => return Reply { hr, data: vec![] } };
            match info.methods.get(&(call.method as u16)) {
                Some(method) => invoke(cpu, &object, method, &call.data),
                None => Reply { hr: NOTIMPL, data: vec![] },
            }
        }
        2 => match qi(cpu, object.pointer, &call.iid) {
            Ok(p) => Reply { hr: 0, data: export(p, call.from, call.iid.clone()).to_le_bytes().to_vec() },
            Err(hr) => Reply { hr, data: vec![] },
        },
        3 | 4 => {
            let value = refcall(cpu, object.pointer, if call.op == 3 { 1 } else { 2 });
            let mut exports = EXPORTS.lock().unwrap();
            if let Some(exports) = exports.as_mut() {
                if call.op == 3 { if let Some(o) = exports.get_mut(&call.object) { o.refs += 1; } }
                else if let Some(o) = exports.get_mut(&call.object) { o.refs -= 1; if o.refs == 0 { exports.remove(&call.object); } }
            }
            Reply { hr: 0, data: value.to_le_bytes().to_vec() }
        }
        _ => Reply { hr: NOTIMPL, data: vec![] },
    }
}
thread_local! { static DEPTH: Cell<u32> = const { Cell::new(0) }; }
pub(crate) fn pump(cpu: &mut koji::Cpu, reentrant: bool) {
    if !REGISTERED.load(Ordering::Relaxed) { return; }
    if DEPTH.with(|d| d.get() >= 16 || !reentrant && d.get() != 0) { return; }
    loop {
        let call = kern::with(|k| {
            let call = k.com.take(crate::pid(), crate::cur().tid);
            if call.is_some() { k.changed(); } call
        }).0;
        let Some(call) = call else { break };
        DEPTH.with(|d| d.set(d.get() + 1));
        let saved = cpu.clone();
        let reply = serve(cpu, &call);
        *cpu = saved;
        DEPTH.with(|d| d.set(d.get() - 1));
        kern::with(|k| { k.com.reply(call.id, reply); k.changed(); });
    }
}
pub(crate) fn wake() {
    if !REGISTERED.load(Ordering::Relaxed) { return; }
    let threads = kern::with(|k| k.com.calls.iter().filter(|c| c.to == crate::pid()).map(|c| c.tid).collect::<std::collections::BTreeSet<_>>()).0;
    for tid in threads { crate::user::post_thread_msg(tid, 0, 0, 0); }
}

#[derive(Clone)]
struct Proxy { pid: u32, tid: u32, object: u32, info: Arc<Interface>, refs: u32 }
static PROXIES: Mutex<Option<HashMap<u32, Proxy>>> = Mutex::new(None);
static VTABLES: Mutex<Vec<(Vec<u8>, u32)>> = Mutex::new(Vec::new());
static METHOD_TRAPS: Mutex<Option<HashMap<u32, u16>>> = Mutex::new(None);
fn request(proxy: &Proxy, op: u32) -> Call {
    Call { from: crate::pid(), to: proxy.pid, tid: proxy.tid, op, object: proxy.object, ..Default::default() }
}
fn proxy(p: u32) -> Option<Proxy> { PROXIES.lock().unwrap().as_ref().and_then(|m| m.get(&p)).cloned() }
fn make_proxy(cpu: &mut koji::Cpu, pid: u32, tid: u32, object: u32, iid: &[u8]) -> Result<u32, u32> {
    let info = interface(cpu, iid)?;
    let cached = VTABLES.lock().unwrap().iter().find(|(g, _)| g == iid).map(|(_, p)| *p);
    let vt = match cached {
        Some(v) => v,
        None => {
            let v = mem::heap_alloc(info.count * ps(), true);
            for (i, (f, bytes)) in [(proxy_qi as api::Handler, 12), (proxy_addref, 4), (proxy_release, 4)].into_iter().enumerate() {
                wp(v + i as u32 * ps(), api::host_fn(&format!("COM proxy {}::{i}", text(iid)), f, bytes));
            }
            for slot in 3..info.count {
                let bytes = info.abi[&(slot as u16)];
                let trap = api::host_fn(&format!("COM proxy {}::{slot}", text(iid)), proxy_method, bytes);
                METHOD_TRAPS.lock().unwrap().get_or_insert_with(HashMap::new).insert(trap, slot as u16);
                wp(v + slot * ps(), trap);
            }
            VTABLES.lock().unwrap().push((iid.to_vec(), v)); v
        }
    };
    let p = mem::heap_alloc(ps(), true); wp(p, vt);
    PROXIES.lock().unwrap().get_or_insert_with(HashMap::new).insert(p, Proxy { pid, tid, object, info, refs: 1 });
    Ok(p)
}
fn proxy_qi(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), guid(c.a(1)), c.a(2));
    if out == 0 { return POINTER; } wp(out, 0);
    let Some(proxy) = proxy(this) else { return DISCONNECTED };
    if iid == UNKNOWN || iid == proxy.info.iid {
        proxy_ref(c.cpu, this, true); wp(out, this); return 0;
    }
    let mut call = request(&proxy, 2); call.iid = iid.clone();
    let reply = call_remote(c.cpu, call);
    if reply.hr != 0 { return reply.hr; }
    let Some(id) = reply.data.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap())) else { return BAD_DATA };
    match make_proxy(c.cpu, proxy.pid, proxy.tid, id, &iid) { Ok(p) => { wp(out, p); 0 }, Err(hr) => hr }
}
fn proxy_ref(cpu: &mut koji::Cpu, this: u32, add: bool) -> u32 {
    let Some(proxy) = proxy(this) else { return 0 };
    let reply = call_remote(cpu, request(&proxy, if add { 3 } else { 4 }));
    let mut proxies = PROXIES.lock().unwrap();
    let Some(map) = proxies.as_mut() else { return 0 };
    let Some(p) = map.get_mut(&this) else { return 0 };
    if add { p.refs += 1; } else { p.refs -= 1; }
    let remaining = p.refs;
    if remaining == 0 { map.remove(&this); mem::heap_free(this); }
    if reply.hr != 0 { 0 } else { remaining }
}
fn proxy_addref(c: &mut Ctx) -> u32 { let this = c.a(0); proxy_ref(c.cpu, this, true) }
fn proxy_release(c: &mut Ctx) -> u32 { let this = c.a(0); proxy_ref(c.cpu, this, false) }
fn proxy_method(c: &mut Ctx) -> u32 {
    let Some(proxy) = proxy(c.a(0)) else { return DISCONNECTED };
    let Some(slot) = METHOD_TRAPS.lock().unwrap().as_ref().and_then(|m| m.get(&c.trap)).copied() else { return NOTIMPL };
    let Some(method) = proxy.info.methods.get(&slot) else { return NOTIMPL };
    let mut args = Vec::new();
    for i in 0..method.stack as u32 / ps() - 1 { args.push(if mem::x64() { c.a64(i) } else { c.a(i) as u64 }); }
    let mut data = Vec::new();
    for p in method.params.iter().filter(|p| !p.retval()) {
        let Some(mut arg) = args.get(p.stack as usize / ps() as usize).copied() else { return BAD_DATA };
        if !mem::x64() && !p.reference && p.kind == Kind::Scalar(8) {
            let Some(high) = args.get(p.stack as usize / 4 + 1) else { return BAD_DATA }; arg |= high << 32;
        }
        match read_value(p, arg, &args, true) { Ok(v) => data.push(v), Err(hr) => return hr }
    }
    let mut call = request(&proxy, 1); call.method = slot as u32; call.data = encoded(&data);
    let reply = call_remote(c.cpu, call);
    if reply.data.is_empty() { return reply.hr; }
    let outputs = match values(&reply.data) { Ok(v) => v, Err(hr) => return hr };
    let params: Vec<&Param> = method.params.iter().filter(|p| !p.retval()).collect();
    if params.len() != outputs.len() { return BAD_DATA; }
    // Validate every output before writing any of them into the caller's memory.
    for (p, (present, bytes)) in params.iter().zip(&outputs) {
        if !p.output() { continue; }
        if !p.reference { return NOTIMPL; }
        if matches!(p.kind, Kind::User { .. }) { if bytes.len() > 8 * 1024 * 1024 { return BAD_DATA; } }
        else if *present && size(p, &args).ok() != Some(bytes.len() as u32) { return BAD_DATA; }
    }
    for (p, (present, bytes)) in params.iter().zip(outputs) {
        if !p.output() { continue; }
        let at = args[p.stack as usize / ps() as usize] as u32;
        if at == 0 { continue; }
        if matches!(p.kind, Kind::User { .. }) { wp(at, if present { bstr(&bytes) } else { 0 }); }
        else if present { mem::write(at, &bytes); }
    }
    reply.hr
}
fn class_for(cpu: &mut koji::Cpu, clsid: &[u8], context: u32) -> Result<Class, u32> {
    let name = text(clsid);
    if let Some(c) = kern::with(|k| k.com.classes.get(&name).filter(|c| !c.suspended).cloned()).0 { return Ok(c); }
    if context & 4 == 0 { return Err(CLASSNOTREG); }
    let app = crate::misc::registry_string(&format!("hkcr\\clsid\\{name}"), "AppID").ok_or(CLASSNOTREG)?;
    let service = crate::misc::registry_string(&format!("hkcr\\appid\\{app}"), "LocalService").ok_or(CLASSNOTREG)?;
    match crate::services::start_named(&service, vec![]) {
        Ok(()) | Err(1056) => {}, Err(e) => return Err(0x8007_0000 | e),
    }
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let (registered, generation) = kern::with(|k| k.com.classes.get(&name).filter(|c| !c.suspended).cloned());
        if let Some(c) = registered { return Ok(c); }
        pump(cpu, true);
        if !kern::wait_change(generation, Some(until)) { return Err(0x8008_0005); }
    }
}
/// Load a registered in-process server and ask its original DllGetClassObject.
pub(crate) fn get_class(cpu: &mut koji::Cpu, clsid: u32, iid: u32, out: u32, context: u32) -> u32 {
    if out == 0 || clsid == 0 || iid == 0 { return POINTER; }
    wp(out, 0);
    if context & 3 != 0 {
        if let Some(path) = crate::misc::registry_string(&format!("hkcr\\clsid\\{}\\inprocserver32", text(&guid(clsid))), "") {
            let base = crate::pe::load_library(cpu, &path);
            if base == 0 { return 0x8007_007e; }
            let Some(entry) = crate::pe::proc_address(base, crate::pe::Import::Name("DllGetClassObject")) else { return 0x8007_007f };
            return crate::call(cpu, entry, &[clsid, iid, out]);
        }
    }
    // Remote factory interfaces need a dedicated activation protocol. Never
    // report success with a null factory when that protocol is unavailable.
    CLASSNOTREG
}
/// Try the registered local-server path after the built-in COM classes.
pub(crate) fn create(cpu: &mut koji::Cpu, clsid: u32, iid: u32, out: u32, context: u32, outer: u32) -> u32 {
    if out == 0 || clsid == 0 || iid == 0 { return POINTER; } wp(out, 0);
    if outer != 0 { return 0x8004_0110; }
    if context & 3 != 0 {
        let factory_iid = mem::heap_alloc(16 + ps(), true);
        let mut iid_bytes = UNKNOWN; iid_bytes[0] = 1;
        mem::write(factory_iid, &iid_bytes);
        let hr = get_class(cpu, clsid, factory_iid, factory_iid + 16, context & 3);
        let factory = rp(factory_iid + 16); mem::heap_free(factory_iid);
        if hr != CLASSNOTREG {
            if hr != 0 { return hr; }
            if factory == 0 { return NOINTERFACE; }
            let result = crate::call(cpu, rp(rp(factory) + 3 * ps()), &[factory, outer, iid, out]);
            refcall(cpu, factory, 2); return result;
        }
    }
    let (class, iid) = (guid(clsid), guid(iid));
    let target = match class_for(cpu, &class, context) { Ok(c) => c, Err(hr) => return hr };
    if target.pid == crate::pid() {
        let Some((factory, _)) = FACTORIES.lock().unwrap().as_ref().and_then(|f| f.get(&target.cookie)).copied() else { return CLASSNOTREG };
        let p = mem::heap_alloc(16, true); mem::write(p, &iid);
        let hr = crate::call(cpu, rp(rp(factory) + 3 * ps()), &[factory, 0, p, out]); mem::heap_free(p); return hr;
    }
    // Read the signature before activation so unsupported interfaces do not leak
    // a server reference when the local proxy cannot be constructed.
    if let Err(hr) = interface(cpu, &iid) { return hr; }
    let reply = call_remote(cpu, Call { from: crate::pid(), to: target.pid, tid: target.tid,
        class: text(&class), iid: iid.clone(), ..Default::default() });
    if reply.hr != 0 { return reply.hr; }
    let Some(object) = reply.data.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap())) else { return BAD_DATA };
    match make_proxy(cpu, target.pid, target.tid, object, &iid) { Ok(p) => { wp(out, p); 0 }, Err(hr) => hr }
}
fn CoRegisterClassObject(c: &mut Ctx) -> u32 {
    let (class, object, context, flags, out) = (guid(c.a(0)), c.a(1), c.a(2), c.a(3), c.a(4));
    if class.len() != 16 || object == 0 || out == 0 { return POINTER; }
    if context & 4 == 0 { return NOTIMPL; }
    let _ = kern::share();
    crate::process::watch();
    let cookie = NEXT_COOKIE.fetch_add(1, Ordering::Relaxed);
    refcall(c.cpu, object, 1);
    FACTORIES.lock().unwrap().get_or_insert_with(HashMap::new).insert(cookie, (object, crate::cur().tid));
    kern::with(|k| { k.com.classes.insert(text(&class), Class { pid: crate::pid(), tid: crate::cur().tid,
        cookie, flags, suspended: flags & 4 != 0 }); k.changed(); });
    REGISTERED.store(true, Ordering::Relaxed); w32(out, cookie); 0
}
fn CoRevokeClassObject(c: &mut Ctx) -> u32 {
    let cookie = c.a(0);
    let object = FACTORIES.lock().unwrap().as_mut().and_then(|m| m.remove(&cookie));
    let Some((object, _)) = object else { return 0x8007_0057 };
    kern::with(|k| { k.com.classes.retain(|_, r| !(r.pid == crate::pid() && r.cookie == cookie)); k.changed(); });
    refcall(c.cpu, object, 2); 0
}
fn suspend(c: &mut Ctx, suspended: bool) -> u32 {
    let _ = c;
    kern::with(|k| {
        for class in k.com.classes.values_mut().filter(|r| r.pid == crate::pid()) { class.suspended = suspended; }
        k.changed();
    }); 0
}
fn CoSuspendClassObjects(c: &mut Ctx) -> u32 { suspend(c, true) }
fn CoResumeClassObjects(c: &mut Ctx) -> u32 { suspend(c, false) }
fn NdrDllRegisterProxy(c: &mut Ctx) -> u32 {
    let (module, list, clsid) = (c.a(0), c.a(1), guid(c.a(2)));
    let Some(path) = crate::pe::with(module, |m| m.path.clone()) else { return 0x8007_0057 };
    if clsid.len() != 16 || list == 0 { return POINTER; }
    let interfaces = read_interfaces(list);
    if interfaces.is_empty() { return NOINTERFACE; }
    let bytes = |s: &str| s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
    let class = text(&clsid);
    if !crate::misc::registry_values(&format!("hkcr\\clsid\\{class}\\inprocserver32"),
        vec![("".into(), 1, bytes(&path)), ("ThreadingModel".into(), 1, bytes("Both"))]) { return 0x8007_0005; }
    for i in &interfaces {
        if !crate::misc::registry_values(&format!("hkcr\\interface\\{}\\proxystubclsid32", text(&i.iid)),
            vec![("".into(), 1, bytes(&class))]) { return 0x8007_0005; }
    }
    add_interfaces(interfaces); 0
}
pub fn init() {
    api::implement("ole32", "CoRegisterClassObject", CoRegisterClassObject);
    api::implement("ole32", "CoRevokeClassObject", CoRevokeClassObject);
    api::implement("ole32", "CoSuspendClassObjects", CoSuspendClassObjects);
    api::implement("ole32", "CoResumeClassObjects", CoResumeClassObjects);
    api::implement("rpcrt4", "NdrDllRegisterProxy", NdrDllRegisterProxy);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn iface(id: u8, count: u32) -> Arc<Interface> {
        Arc::new(Interface { iid: vec![id; 16], count, methods: BTreeMap::new(), abi: BTreeMap::new() })
    }
    /// Loading the same proxy DLL again (an IID it does not have, NdrDllRegisterProxy per
    /// call) does not grow the list; the first registration of an IID stays; a miss is
    /// forgotten once some DLL registers that IID
    #[test]
    fn interfaces_are_added_once() {
        let base = INTERFACES.lock().unwrap().len();
        for _ in 0..5 {
            add_interfaces(vec![iface(0xa1, 4), iface(0xa2, 5)]);
        }
        add_interfaces(vec![iface(0xa1, 9)]);
        let l = INTERFACES.lock().unwrap();
        assert_eq!(l.len(), base + 2);
        assert_eq!(l.iter().find(|i| i.iid == [0xa1; 16]).unwrap().count, 4);
        drop(l);
        MISSES.lock().unwrap().insert(vec![0xa3; 16]);
        add_interfaces(vec![iface(0xa3, 3)]);
        assert!(!MISSES.lock().unwrap().contains(&[0xa3; 16][..]));
    }
}
