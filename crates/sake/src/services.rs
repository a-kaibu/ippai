//! User-mode own-process services. Configuration lives in the guest registry;
//! process ownership, status and control requests live in the shared kernel.
#![allow(non_snake_case)]
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use crate::api::{implement, Ctx};
use crate::kern::{self, services::Service};
use crate::mem::{self, r32, w32};
use crate::obj::{self, Obj};

#[derive(Default)]
struct Handles {
    managers: HashMap<u32, u32>,
    services: HashMap<u32, (String, u32)>,
}
static HANDLES: Mutex<Option<Handles>> = Mutex::new(None);
fn handles<R>(f: impl FnOnce(&mut Handles) -> R) -> R {
    f(HANDLES.lock().unwrap().get_or_insert_with(Handles::default))
}
fn fail(error: u32) -> u32 {
    if crate::env_on!("SAKE_TRACE_SERVICES") { eprintln!("sake: SCM error {error}"); }
    crate::k32::last_error(error); 0
}
fn string(p: u32, wide: bool) -> String { if p == 0 { String::new() } else { crate::k32::arg_str(p, wide) } }
fn path(name: &str) -> String { format!("hklm\\system\\currentcontrolset\\services\\{name}") }
fn ensure(name: &str) -> Result<(), u32> {
    let key = name.to_ascii_lowercase();
    if let Some(deleted) = kern::with(|k| k.services.get(&key).map(|s| s.deleted)).0 {
        return if deleted { Err(1072) } else { Ok(()) };
    }
    let p = path(&key);
    let number = |name| crate::misc::registry_value(&p, name).and_then(|(ty, b)|
        (ty == 4 && b.len() == 4).then(|| u32::from_le_bytes(b.try_into().unwrap())));
    let ty = number("Type").ok_or(1060u32)?;
    let start = number("Start").unwrap_or(3);
    let image = crate::misc::registry_string(&p, "ImagePath").ok_or(1060u32)?;
    let display = crate::misc::registry_string(&p, "DisplayName").unwrap_or_else(|| name.into());
    let dependencies = crate::misc::registry_value(&p, "DependOnService").filter(|(ty, _)| *ty == 7).map(|(_, b)| {
        let words: Vec<u16> = b.chunks_exact(2).map(|v| u16::from_le_bytes([v[0],v[1]])).collect();
        words.split(|v| *v == 0).filter(|v| !v.is_empty()).map(String::from_utf16_lossy).collect::<Vec<_>>()
    }).unwrap_or_default();
    kern::with(|k| {
        if !k.services.contains_key(&key) {
            let mut s = Service::new(name.into(), display, ty, start, image); s.dependencies = dependencies;
            k.services.insert(key, s); k.changed();
        }
    });
    Ok(())
}
fn service(h: u32, access: u32) -> Result<String, u32> {
    let (name, granted) = handles(|hnd| hnd.services.get(&h).cloned()).ok_or(6u32)?;
    if granted & access != access { return Err(5); }
    Ok(name)
}
fn open_manager(c: &mut Ctx, wide: bool) -> u32 {
    let machine = string(c.a(0), wide);
    if !machine.is_empty() && machine != "." && !machine.eq_ignore_ascii_case("localhost") { return fail(1722); }
    let access = c.a(2);
    let h = obj::add(Obj::Other("service manager"));
    handles(|m| m.managers.insert(h, access)); h
}
fn OpenSCManagerA(c: &mut Ctx) -> u32 { open_manager(c, false) }
fn OpenSCManagerW(c: &mut Ctx) -> u32 { open_manager(c, true) }
fn new_handle(name: String, access: u32) -> u32 {
    let h = obj::add(Obj::Other("service"));
    let opened = kern::with(|k| {
        let s = k.services.get_mut(&name).ok_or(1060u32)?;
        if s.deleted { return Err(1072); }
        *s.refs.entry(crate::pid()).or_default() += 1; k.changed(); Ok(())
    }).0;
    if let Err(e) = opened { obj::close(h); return fail(e); }
    handles(|m| m.services.insert(h, (name, access))); h
}
fn open_service(c: &mut Ctx, wide: bool) -> u32 {
    if !handles(|m| m.managers.contains_key(&c.a(0))) { return fail(6); }
    let name = string(c.a(1), wide).to_ascii_lowercase();
    if let Err(e) = ensure(&name) { return fail(e); }
    new_handle(name, c.a(2))
}
fn OpenServiceA(c: &mut Ctx) -> u32 { open_service(c, false) }
fn OpenServiceW(c: &mut Ctx) -> u32 { open_service(c, true) }
fn create_service(c: &mut Ctx, wide: bool) -> u32 {
    let Some(access) = handles(|m| m.managers.get(&c.a(0)).copied()) else { return fail(6) };
    if access & 2 == 0 { return fail(5); }
    let name = string(c.a(1), wide);
    if name.is_empty() || name.contains(['/', '\\']) || name.len() > 256 { return fail(123); }
    let key = name.to_ascii_lowercase();
    match ensure(&key) { Ok(()) => return fail(1073), Err(1072) => return fail(1072), _ => {} }
    let display = if c.a(2) == 0 { name.clone() } else { string(c.a(2), wide) };
    let (ty, start, image) = (c.a(4), c.a(5), string(c.a(7), wide));
    if image.is_empty() || start > 4 || ty & !(0x100 | 0x10 | 0x20 | 3) != 0 { return fail(87); }
    if !string(c.a(8), wide).is_empty() { return fail(120); }
    let mut dependencies = Vec::new();
    let mut dep = c.a(10);
    if dep != 0 {
        for _ in 0..256 {
            let name = string(dep, wide); if name.is_empty() { break; }
            dep += if wide { (mem::wstr(dep).len() as u32 + 1) * 2 } else { mem::cstr(dep).len() as u32 + 1 };
            dependencies.push(name);
        }
    }
    let account = string(c.a(11), wide);
    if !account.is_empty() && !account.eq_ignore_ascii_case("LocalSystem") { return fail(1069); }
    let utf16 = |s: &str| s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
    if !crate::misc::registry_values(&path(&key), vec![
        ("Type".into(), 4, ty.to_le_bytes().to_vec()), ("Start".into(), 4, start.to_le_bytes().to_vec()),
        ("ErrorControl".into(), 4, c.a(6).to_le_bytes().to_vec()),
        ("ImagePath".into(), 2, utf16(&image)), ("DisplayName".into(), 1, utf16(&display)),
        ("DependOnService".into(), 7, dependencies.iter().flat_map(|s| s.encode_utf16().chain([0])).chain([0]).flat_map(u16::to_le_bytes).collect()),
    ]) { return fail(5); }
    let inserted = kern::with(|k| {
        if k.services.contains_key(&key) { return false; }
        let mut s = Service::new(name, display, ty, start, image); s.dependencies = dependencies;
        k.services.insert(key.clone(), s); k.changed(); true
    }).0;
    if !inserted { return fail(1073); }
    mem::put32(c.a(9), 0);
    new_handle(key, c.a(3))
}
fn CreateServiceA(c: &mut Ctx) -> u32 { create_service(c, false) }
fn CreateServiceW(c: &mut Ctx) -> u32 { create_service(c, true) }

pub(crate) fn start_named(name: &str, args: Vec<String>) -> Result<(), u32> {
    start_dependencies(name, args, &mut Vec::new())
}
fn start_dependencies(name: &str, args: Vec<String>, chain: &mut Vec<String>) -> Result<(), u32> {
    ensure(name)?;
    let key = name.to_ascii_lowercase();
    if chain.contains(&key) { return Err(1059); }
    chain.push(key.clone());
    let dependencies = kern::with(|k| k.services.get(&key).map(|s| s.dependencies.clone()).unwrap_or_default()).0;
    for dep in dependencies {
        // RpcSs is supplied by sake's COM activation/call broker, like the core
        // Windows DLLs are supplied by HLE. It has no guest executable to start.
        if dep.eq_ignore_ascii_case("RpcSs") { continue; }
        if dep.starts_with('+') { return Err(1068); }
        ensure(&dep).map_err(|_| 1075u32)?;
        let dep_key = dep.to_ascii_lowercase();
        if kern::with(|k| k.services.get(&dep_key).is_some_and(|s| s.status[1] == 4)).0 { continue; }
        match start_dependencies(&dep, vec![], chain) { Ok(()) | Err(1056) => {}, Err(e) => return Err(e) }
        kern::block(Some(Instant::now() + Duration::from_secs(30)), &|| false, |k| {
            let s = k.services.get(&dep_key).ok_or(1068u32)?;
            match s.status[1] { 4 => Ok(()), 1 => Err(1068), _ => Err(kern::PENDING) }
        }).map_err(|_| 1068u32)?;
    }
    chain.pop();
    let command = kern::with(|k| {
        let s = k.services.get_mut(&key).ok_or(1060u32)?;
        if s.ty & 0x20 != 0 { return Err(120u32); } // shared-process hosting is not implemented
        s.begin_start(args)?;
        let command = s.path.clone(); k.changed(); Ok(command)
    }).0?;
    let pid = match crate::process::spawn_service(&crate::k32::expand_env(&command), &key) {
        Ok(pid) => pid,
        Err(e) => {
            kern::with(|k| { if let Some(s) = k.services.get_mut(&key) {
                s.status = vec![s.ty, 1, 0, e, 0, 0, 0]; s.pid = 0; k.changed();
            }});
            return Err(e);
        }
    };
    let until = Instant::now() + Duration::from_secs(30);
    kern::block(Some(until), &|| false, |k| {
        let s = k.services.get(&key).ok_or(1060u32)?;
        if s.connected { return Ok(()); }
        if k.exit_code(pid).is_some() || s.pid != pid { return Err(1067); }
        Err(kern::PENDING)
    }).map_err(|e| if e == kern::IO_TIMEOUT { 1053 } else { e })
}
fn start_service(c: &mut Ctx, wide: bool) -> u32 {
    let name = match service(c.a(0), 0x10) { Ok(n) => n, Err(e) => return fail(e) };
    let (n, p) = (c.a(1), c.a(2));
    if n > 1024 || n != 0 && p == 0 { return fail(87); }
    let args = (0..n).map(|i| string(mem::rp(p + i * mem::ps()), wide)).collect();
    match start_named(&name, args) { Ok(()) => 1, Err(e) => fail(e) }
}
fn StartServiceA(c: &mut Ctx) -> u32 { start_service(c, false) }
fn StartServiceW(c: &mut Ctx) -> u32 { start_service(c, true) }
fn status(name: &str, out: u32, extended: bool) -> Result<(), u32> {
    if out == 0 { return Err(87); }
    let s = kern::with(|k| k.services.get(name).cloned()).0.ok_or(6u32)?;
    for (i, value) in s.status.iter().enumerate() { w32(out + 4 * i as u32, *value); }
    if extended { w32(out + 28, s.pid); w32(out + 32, 0); }
    Ok(())
}
fn QueryServiceStatus(c: &mut Ctx) -> u32 {
    match service(c.a(0), 4).and_then(|n| status(&n, c.a(1), false)) { Ok(()) => 1, Err(e) => fail(e) }
}
fn QueryServiceStatusEx(c: &mut Ctx) -> u32 {
    let name = match service(c.a(0), 4) { Ok(n) => n, Err(e) => return fail(e) };
    if c.a(1) != 0 { return fail(124); }
    mem::put32(c.a(4), 36);
    if c.a(3) < 36 { return fail(122); }
    match status(&name, c.a(2), true) { Ok(()) => 1, Err(e) => fail(e) }
}
fn ControlService(c: &mut Ctx) -> u32 {
    let code = c.a(1);
    let access = match code { 1 => 0x20, 2 | 3 => 0x40, 4 => 0x80, 128..=255 => 0x100, _ => return fail(87) };
    let name = match service(c.a(0), access) { Ok(n) => n, Err(e) => return fail(e) };
    let id = kern::with(|k| {
        let s = k.services.get_mut(&name).ok_or(6u32)?;
        if s.status[1] == 1 { return Err(1062); }
        let mask = match code { 1 => 1, 2 | 3 => 2, _ => 0 };
        if s.status[1] != 4 && s.status[1] != 7 || s.status[2] & mask != mask { return Err(1061); }
        s.next_control += 1;
        let id = s.next_control;
        s.controls.push_back((id, code)); k.changed(); Ok(id)
    }).0;
    let id = match id { Ok(id) => id, Err(e) => return fail(e) };
    let reply = kern::block(Some(Instant::now() + Duration::from_secs(30)), &|| false, |k| {
        let s = k.services.get_mut(&name).ok_or(1062u32)?;
        if let Some(result) = s.replies.remove(&id) { k.changed(); return Ok(result); }
        if s.status[1] == 1 { return Err(1062); }
        Err(kern::PENDING)
    });
    match reply {
        Ok(0) => match status(&name, c.a(2), false) { Ok(()) => 1, Err(e) => fail(e) },
        Ok(e) => fail(e), Err(e) => fail(if e == kern::IO_TIMEOUT { 1053 } else { e }),
    }
}
fn DeleteService(c: &mut Ctx) -> u32 {
    let name = match service(c.a(0), 0x10000) { Ok(n) => n, Err(e) => return fail(e) };
    if !crate::misc::registry_remove_tree(&path(&name)) { return fail(5); }
    let result = kern::with(|k| {
        let s = k.services.get_mut(&name).ok_or(6u32)?;
        if s.deleted { return Err(1072); }
        s.deleted = true; k.changed(); Ok(())
    }).0;
    match result { Ok(()) => 1, Err(e) => fail(e) }
}
fn CloseServiceHandle(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let (manager, record) = handles(|m| (m.managers.remove(&h).is_some(), m.services.remove(&h)));
    if let Some((name, _)) = record {
        kern::with(|k| {
            if let Some(s) = k.services.get_mut(&name) {
                if let Some(count) = s.refs.get_mut(&crate::pid()) { *count -= 1; if *count == 0 { s.refs.remove(&crate::pid()); } }
                if s.removable() { k.services.remove(&name); }
                k.changed();
            }
        });
    } else if !manager { return fail(6); }
    obj::close(h); 1
}
fn query_config(c: &mut Ctx, wide: bool) -> u32 {
    let name = match service(c.a(0), 1) { Ok(n) => n, Err(e) => return fail(e) };
    let Some(s) = kern::with(|k| k.services.get(&name).cloned()).0 else { return fail(6) };
    let encode = |s: &str| -> Vec<u8> {
        if wide { s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect() }
        else { let mut b = crate::text::s2a(s); b.push(0); b }
    };
    let strings = [encode(&s.path), encode(""), encode(""), encode("LocalSystem"), encode(&s.display)];
    let fixed = if mem::x64() { 64 } else { 36 };
    let total = fixed + strings.iter().map(|s| s.len() as u32).sum::<u32>();
    mem::put32(c.a(3), total);
    let out = c.a(1);
    if out == 0 || c.a(2) < total { return fail(122); }
    mem::fill(out, fixed, 0); w32(out, s.ty); w32(out + 4, s.start); w32(out + 8, 1);
    let offsets = if mem::x64() { [16, 24, 40, 48, 56] } else { [12, 16, 24, 28, 32] };
    let mut at = out + fixed;
    for (offset, bytes) in offsets.into_iter().zip(strings) { mem::wp(out + offset, at); mem::write(at, &bytes); at += bytes.len() as u32; }
    1
}
fn QueryServiceConfigA(c: &mut Ctx) -> u32 { query_config(c, false) }
fn QueryServiceConfigW(c: &mut Ctx) -> u32 { query_config(c, true) }

struct Active { name: String, main: u32, wide: bool, handler: u32, extended: bool, context: u32, token: u32 }
static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);
fn main_thread(c: &mut Ctx) -> u32 {
    let (name, main, wide) = { let a = ACTIVE.lock().unwrap(); let a = a.as_ref().unwrap(); (a.name.clone(), a.main, a.wide) };
    let Some(s) = kern::with(|k| k.services.get(&name).cloned()).0 else { return 0 };
    let args: Vec<String> = std::iter::once(s.name).chain(s.args).collect();
    let argv = mem::heap_alloc((args.len() as u32 + 1) * mem::ps(), true);
    let mut allocated = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        let b = if wide { arg.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect::<Vec<u8>>() }
            else { let mut b = crate::text::s2a(arg); b.push(0); b };
        let p = mem::heap_alloc(b.len() as u32, false); mem::write(p, &b); mem::wp(argv + i as u32 * mem::ps(), p);
        allocated.push(p);
    }
    crate::call(c.cpu, main, &[args.len() as u32, argv]);
    for p in allocated { mem::heap_free(p); } mem::heap_free(argv);
    kern::with(|k| { if let Some(s) = k.services.get_mut(&name) {
        if s.pid == crate::pid() && s.status[1] != 1 { s.exited(crate::pid()); k.changed(); }
    }});
    0
}
fn dispatcher(c: &mut Ctx, wide: bool) -> u32 {
    let Some(name) = crate::process::record().and_then(|p| p.service) else { return fail(1063) };
    let table = c.a(0);
    if table == 0 { return fail(87); }
    let mut main = 0;
    for i in 0..256 {
        let p = table + i * mem::ps() * 2;
        let (n, entry) = (mem::rp(p), mem::rp(p + mem::ps()));
        if entry == 0 { break; }
        if string(n, wide).eq_ignore_ascii_case(&name) { main = entry; break; }
    }
    if main == 0 { return fail(1083); }
    {
        let mut active = ACTIVE.lock().unwrap();
        if active.is_some() { return fail(1056); }
        *active = Some(Active { name: name.clone(), main, wide, handler: 0, extended: false, context: 0, token: 0 });
    }
    let connected = kern::with(|k| {
        let Some(s) = k.services.get_mut(&name) else { return false };
        if s.pid != crate::pid() { return false; }
        s.connected = true; k.changed(); true
    }).0;
    if !connected { *ACTIVE.lock().unwrap() = None; return fail(1063); }
    let entry = crate::api::host_fn("service main thread", main_thread, 4);
    let thread = obj::new_thread_obj(0);
    crate::spawn_gated(entry, 0, 0x100000, thread, false);
    loop {
        let (work, generation) = kern::with(|k| {
            let Some(s) = k.services.get_mut(&name) else { return (true, None) };
            let stopped = s.status[1] == 1;
            let request = s.controls.pop_front();
            if request.is_some() { k.changed(); }
            (stopped, request)
        });
        if let Some((id, code)) = work.1 {
            let (handler, extended, context) = {
                let active = ACTIVE.lock().unwrap(); let a = active.as_ref().unwrap(); (a.handler, a.extended, a.context)
            };
            let result = if handler == 0 { 1061 } else if extended {
                crate::call(c.cpu, handler, &[code, 0, 0, context])
            } else { crate::call(c.cpu, handler, &[code]); 0 };
            kern::with(|k| { if let Some(s) = k.services.get_mut(&name) { s.replies.insert(id, result); k.changed(); }});
        } else if work.0 { break; } else { kern::wait_change(generation, None); }
    }
    // ServiceMain can still be returning after its last STOPPED report. Keep its
    // active record alive until the guest thread has completed.
    obj::wait(&[thread], false, u32::MAX, None);
    obj::close(thread);
    if let Some(a) = ACTIVE.lock().unwrap().take() { if a.token != 0 { obj::close(a.token); } }
    1
}
fn StartServiceCtrlDispatcherA(c: &mut Ctx) -> u32 { dispatcher(c, false) }
fn StartServiceCtrlDispatcherW(c: &mut Ctx) -> u32 { dispatcher(c, true) }
fn register(c: &mut Ctx, wide: bool, extended: bool) -> u32 {
    let requested = string(c.a(0), wide);
    let mut active = ACTIVE.lock().unwrap();
    let Some(a) = active.as_mut() else { return fail(1063) };
    if !requested.eq_ignore_ascii_case(&a.name) { return fail(1083); }
    if c.a(1) == 0 { return fail(87); }
    a.handler = c.a(1); a.extended = extended; a.context = if extended { c.a(2) } else { 0 };
    if a.token == 0 { a.token = obj::add(Obj::Other("service status")); }
    a.token
}
fn RegisterServiceCtrlHandlerA(c: &mut Ctx) -> u32 { register(c, false, false) }
fn RegisterServiceCtrlHandlerW(c: &mut Ctx) -> u32 { register(c, true, false) }
fn RegisterServiceCtrlHandlerExA(c: &mut Ctx) -> u32 { register(c, false, true) }
fn RegisterServiceCtrlHandlerExW(c: &mut Ctx) -> u32 { register(c, true, true) }
fn SetServiceStatus(c: &mut Ctx) -> u32 {
    let name = {
        let active = ACTIVE.lock().unwrap();
        let Some(a) = active.as_ref().filter(|a| a.token != 0 && a.token == c.a(0)) else { return fail(6) };
        a.name.clone()
    };
    if c.a(1) == 0 { return fail(87); }
    let words = (0..7).map(|i| r32(c.a(1) + 4 * i)).collect();
    let result = kern::with(|k| {
        let s = k.services.get_mut(&name).ok_or(6u32)?;
        s.report(crate::pid(), words)?; k.changed(); Ok(())
    }).0;
    match result { Ok(()) => 1, Err(e) => fail(e) }
}
pub fn init() {
    macro_rules! reg { ($($f:ident),* $(,)?) => { $(implement("advapi32", stringify!($f), $f);)* }; }
    reg!(OpenSCManagerA, OpenSCManagerW, OpenServiceA, OpenServiceW, CreateServiceA, CreateServiceW,
        StartServiceA, StartServiceW, ControlService, QueryServiceStatus, QueryServiceStatusEx,
        DeleteService, CloseServiceHandle, QueryServiceConfigA, QueryServiceConfigW,
        StartServiceCtrlDispatcherA, StartServiceCtrlDispatcherW, RegisterServiceCtrlHandlerA,
        RegisterServiceCtrlHandlerW, RegisterServiceCtrlHandlerExA, RegisterServiceCtrlHandlerExW, SetServiceStatus);
}
