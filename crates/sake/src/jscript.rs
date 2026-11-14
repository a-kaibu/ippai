//! JScript: the Active Scripting engine (CLSID_JScript, JScript9 / Chakra's legacy CLSID) on
//! boa, a JavaScript engine in Rust. IActiveScript + IActiveScriptParse run script text in one
//! global scope; a named item (AddNamedItem, its object from the site's GetItemInfo) becomes a
//! global. Host objects (IDispatch) reach scripts as proxies whose member reads call
//! GetIDsOfNames + Invoke(PROPERTYGET) and whose methods call Invoke(METHOD); script objects
//! and functions reach the host as IDispatch / IDispatchEx objects. Errors go to the site's
//! OnScriptError (and the log).

#![allow(non_snake_case)]

use crate::api::{vtable, Ctx};
use crate::mem::{self, put32, r16, r32, w16, w32};
use crate::typelib::{read_var, write_var, Var};
use crate::HashMap;
use boa_engine::object::builtins::JsProxyBuilder;
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::property::PropertyDescriptor;
use boa_engine::{js_string, Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, Source};
use std::cell::{Cell, RefCell};

const S_OK: u32 = 0;
const S_FALSE: u32 = 1;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_INVALIDARG: u32 = 0x8007_0057;
const E_UNEXPECTED: u32 = 0x8000_ffff;
const DISP_E_MEMBERNOTFOUND: u32 = 0x8002_0003;
const DISP_E_EXCEPTION: u32 = 0x8002_0009;
const SCRIPT_E_REPORTED: u32 = 0x8002_0101;

const DISPATCH_METHOD: u32 = 1;
const DISPATCH_PROPERTYGET: u32 = 2;
const DISPATCH_PROPERTYPUT: u32 = 4;

/// SCRIPTSTATE_*
const STARTED: u32 = 1;
const CONNECTED: u32 = 2;
const CLOSED: u32 = 4;
const INITIALIZED: u32 = 5;

struct Engine {
    ctx: *mut Context,
    site: u32,
    state: u32,
    /// named items not made globals yet: (name, flags)
    items: Vec<(Vec<u16>, u32)>,
    /// script text parsed before the engine was started
    pending: Vec<Vec<u16>>,
    /// IActiveScript, IActiveScriptParse32, IObjectSafety, IActiveScriptProperty
    blocks: [u32; 4],
    refs: u32,
}

/// How deep script -> host -> script calls may nest: each level is boa's frames plus a guest
/// call on the thread's fixed native stack, which runs out long before boa's own limits. Past
/// it a call into the host throws (JScript's "Out of stack space") instead.
pub const MAX_DEPTH: u32 = 48;
/// JScript's "Out of stack space" (800A001C)
const OUT_OF_STACK: u32 = 0x800a_001c;

#[derive(Default)]
struct Tables {
    engines: HashMap<u32, Engine>,
    next: u32,
    /// (engine, host IDispatch) -> its proxy (holding one reference to it), and back
    proxies: HashMap<(u32, u32), JsObject>,
    hosts: HashMap<JsObject, u32>,
    /// script object -> its IDispatch block, and back (block -> (engine, object, refs))
    disps: HashMap<JsObject, u32>,
    objects: HashMap<u32, (u32, JsObject, u32)>,
    /// (host vtable, name) -> (DISPID, a method rather than a property)
    members: HashMap<(u32, String), Option<(u32, bool)>>,
    /// DISPIDs given to names of script objects (from 1000), and back
    names: Vec<String>,
    name_ids: HashMap<String, u32>,
}

thread_local! {
    static T: RefCell<Tables> = RefCell::new(Tables::default());
    /// the Ctx of the sake call script code runs under (for calls into guest code)
    static CUR: Cell<*mut Ctx<'static>> = const { Cell::new(std::ptr::null_mut()) };
    /// how many `enter`s are running (an engine is freed only outside all of them)
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

fn t<R>(f: impl FnOnce(&mut Tables) -> R) -> R {
    T.with(|t| f(&mut t.borrow_mut()))
}

/// Runs `f` with `c` as the Ctx script code calls guest code through.
fn enter<R>(c: &mut Ctx, f: impl FnOnce() -> R) -> R {
    let prev = CUR.with(|p| p.replace(c as *mut Ctx as *mut Ctx<'static>));
    DEPTH.with(|d| d.set(d.get() + 1));
    let r = f();
    let outer = DEPTH.with(|d| {
        d.set(d.get() - 1);
        d.get() == 0
    });
    // (engines whose last reference went while script ran: freed now that none runs)
    if outer {
        let dead: Vec<u32> = t(|t| t.engines.iter().filter(|(_, e)| e.refs == 0).map(|(&id, _)| id).collect());
        for id in dead {
            maybe_free(id);
        }
    }
    CUR.with(|p| p.set(prev));
    r
}
fn depth() -> u32 {
    DEPTH.with(|d| d.get())
}
/// Calls guest code from script code.
fn gcall(f: u32, args: &[u32]) -> u32 {
    let c = CUR.with(|p| p.get());
    assert!(!c.is_null(), "jscript: guest call outside a sake call");
    // SAFETY: CUR is the live Ctx of the sake call that is running this script
    unsafe { (*c).call(f, args) }
}
/// Calls method `slot` of COM object `obj`.
fn com(obj: u32, slot: u32, args: &[u32]) -> u32 {
    let mut a = vec![obj];
    a.extend_from_slice(args);
    gcall(mem::rp(mem::rp(obj) + mem::ps() * slot), &a)
}

fn engine_ctx(id: u32) -> Option<&'static mut Context> {
    let p = t(|t| t.engines.get(&id).map(|e| e.ctx))?;
    // SAFETY: the Context lives (boxed) until the engine is closed; script code reentering
    // the engine (a host call calling back into script) runs on the same thread in between
    // the outer call's own uses of it.
    Some(unsafe { &mut *p })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

// ---------------------------------------------------------------------------------------------
// values

/// `v` as a script value for engine `engine` (`owned`: the VARIANT's object reference is ours,
/// as an Invoke result's; else borrowed, as an argument's, and a proxy made for it takes one).
fn to_js(v: &Var, ctx: &mut Context, engine: u32, owned: bool) -> JsValue {
    match v {
        Var::Empty | Var::Error(_) => JsValue::undefined(),
        Var::Null => JsValue::null(),
        Var::I4(i) => JsValue::new(*i),
        Var::R8(f) => JsValue::new(*f),
        Var::Bool(b) => JsValue::new(*b),
        Var::Str(s) => JsValue::new(JsString::from(&s[..])),
        Var::Obj(0, _) => JsValue::null(),
        Var::Obj(p, _) => {
            if let Some(o) = t(|t| t.objects.get(p).map(|x| x.1.clone())) {
                // (one of ours back: the reference that came with it is let go)
                if owned {
                    unref_disp(*p);
                }
                return o.into();
            }
            host_proxy(engine, *p, ctx, owned).map_or(JsValue::null(), JsValue::from)
        }
    }
}

/// `v` as a VARIANT value (`owned`: the receiver keeps object references, so they are
/// counted for it).
fn from_js(v: &JsValue, engine: u32, owned: bool) -> Var {
    if let Some(p) = primitive(v) {
        return p;
    }
    if let Some(o) = v.as_object() {
        if let Some(p) = t(|t| t.hosts.get(&o).copied()) {
            if owned {
                com(p, 1, &[]);
            }
            return Var::Obj(p, true);
        }
        return Var::Obj(script_disp(engine, o, owned), true);
    }
    Var::Empty
}

/// A primitive value as a VARIANT value (None for objects).
fn primitive(v: &JsValue) -> Option<Var> {
    if v.is_undefined() {
        return Some(Var::Empty);
    }
    if v.is_null() {
        return Some(Var::Null);
    }
    if let Some(b) = v.as_boolean() {
        return Some(Var::Bool(b));
    }
    if let Some(n) = v.as_number() {
        let int = n.fract() == 0. && n >= i32::MIN as f64 && n <= i32::MAX as f64 && !(n == 0. && n.is_sign_negative());
        return Some(if int { Var::I4(n as i32) } else { Var::R8(n) });
    }
    if let Some(s) = v.as_string() {
        return Some(Var::Str(s.to_vec()));
    }
    (!v.is_object()).then_some(Var::Empty)
}

fn throw(msg: String) -> JsError {
    JsNativeError::error().with_message(msg).into()
}

// ---------------------------------------------------------------------------------------------
// host objects in scripts

const DISP_KEY: &str = "\u{1}disp";

/// The proxy of host object `p` in engine `engine`; it holds one reference: a new proxy keeps
/// the reference that came with `p` if `owned`, else takes one; an existing proxy lets an
/// owned one go (each Invoke that returns the object AddRefs it: those were never released).
fn host_proxy(engine: u32, p: u32, ctx: &mut Context, owned: bool) -> Option<JsObject> {
    if let Some(o) = t(|t| t.proxies.get(&(engine, p)).cloned()) {
        if owned {
            com(p, 2, &[]);
        }
        return Some(o);
    }
    let target = JsObject::with_null_proto();
    target
        .define_property_or_throw(js_string!(DISP_KEY), PropertyDescriptor::builder().value(p as f64).writable(false).enumerable(false).configurable(false), ctx)
        .ok()?;
    let proxy = JsProxyBuilder::new(target).get(host_get).set(host_set).build(ctx).ok()?;
    let o: JsObject = proxy.into();
    if !owned {
        com(p, 1, &[]);
    }
    t(|t| {
        t.proxies.insert((engine, p), o.clone());
        t.hosts.insert(o.clone(), p);
    });
    Some(o)
}

fn proxied(target: &JsValue, ctx: &mut Context) -> JsResult<u32> {
    let o = target.as_object().ok_or_else(|| throw("not a host object".into()))?;
    Ok(o.get(js_string!(DISP_KEY), ctx)?.as_number().unwrap_or(0.) as u32)
}

fn key_name(k: &JsValue, ctx: &mut Context) -> JsResult<Option<String>> {
    if k.is_symbol() {
        return Ok(None);
    }
    Ok(Some(k.to_string(ctx)?.to_std_string_escaped()))
}

/// (DISPID, is a method) of member `name` of host object `p`.
fn member(p: u32, name: &str) -> Option<(u32, bool)> {
    let key = (r32(p), name.to_string());
    if let Some(m) = t(|t| t.members.get(&key).copied()) {
        return m;
    }
    let s = mem::heap_alloc(2 * name.len() as u32 + 8, true);
    for (i, ch) in name.encode_utf16().enumerate() {
        w16(s + 2 * i as u32, ch);
    }
    let names = mem::heap_alloc(8, true);
    w32(names, s);
    let id = names + 4;
    // GetIDsOfNames(riid, names, 1, lcid, ids)
    let hr = com(p, 5, &[iid_null(), names, 1, 0x411, id]);
    let r = (hr == S_OK).then(|| r32(id)).map(|id| {
        // a member a property get fails on is a method
        let dp = mem::heap_alloc(16, true);
        let res = mem::heap_alloc(16, true);
        let hr = com(p, 6, &[id, iid_null(), 0x411, DISPATCH_PROPERTYGET, dp, res, 0, 0]);
        let method = hr != S_OK;
        if !method {
            clear_var(res);
        }
        mem::heap_free(dp);
        mem::heap_free(res);
        (id, method)
    });
    mem::heap_free(s);
    mem::heap_free(names);
    t(|t| t.members.insert(key, r));
    r
}

fn iid_null() -> u32 {
    static P: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *P.get_or_init(|| mem::heap_alloc(16, true))
}

/// VariantClear for a VARIANT sake wrote or got back.
fn clear_var(p: u32) {
    match r16(p) {
        8 => crate::misc::bstr_free(r32(p + 8)),
        9 | 13 if r32(p + 8) != 0 => {
            com(r32(p + 8), 2, &[]);
        }
        _ => {}
    }
    w16(p, 0);
}

/// Invoke on a host object with script arguments; the result as a script value.
fn host_invoke(p: u32, id: u32, flags: u32, args: &[JsValue], named_put: bool, ctx: &mut Context) -> JsResult<JsValue> {
    if depth() >= MAX_DEPTH {
        let e = JsNativeError::range().with_message("Out of stack space").into_opaque(ctx);
        let _ = e.set(js_string!("number"), JsValue::new(OUT_OF_STACK as i32), false, ctx);
        return Err(JsError::from_opaque(e.into()));
    }
    let engine = current_engine();
    let n = args.len() as u32;
    let rg = mem::heap_alloc(16 * n.max(1), true);
    // reversed, as DISPPARAMS wants them; script objects passed hold a reference for the call
    // only (the host AddRefs one it keeps)
    let mut lent = Vec::new();
    for (i, a) in args.iter().enumerate() {
        let v = from_js(a, engine, false);
        if let Var::Obj(b, _) = v {
            if t(|t| t.objects.contains_key(&b)) {
                lent.push(b);
            }
        }
        write_var(rg + 16 * (n - 1 - i as u32), &v);
    }
    let dp = mem::heap_alloc(16 + 4, true);
    w32(dp, rg);
    w32(dp + 8, n);
    if named_put {
        w32(dp + 16, (-3i32) as u32);
        w32(dp + 4, dp + 16);
        w32(dp + 12, 1);
    }
    let res = mem::heap_alloc(16, true);
    let exc = mem::heap_alloc(32, true);
    let hr = com(p, 6, &[id, iid_null(), 0x411, flags, dp, res, exc, 0]);
    let v = read_var(res);
    let out = to_js(&v, ctx, engine, true);
    // the result's own references: strings are copied, objects are kept by the proxy
    if r16(res) == 8 {
        crate::misc::bstr_free(r32(res + 8));
    }
    for i in 0..n {
        let a = rg + 16 * i;
        if r16(a) == 8 {
            crate::misc::bstr_free(r32(a + 8));
        }
    }
    let desc = r32(exc + 8);
    let scode = r32(exc + 28);
    let msg = if hr != S_OK && desc != 0 { String::from_utf16_lossy(&mem::wstr(desc)) } else { String::new() };
    // the EXCEPINFO's strings are the caller's to free (bstrSource, bstrDescription,
    // bstrHelpFile)
    for o in [4, 8, 12] {
        let b = r32(exc + o);
        if b != 0 {
            crate::misc::bstr_free(b);
        }
    }
    mem::heap_free(rg);
    mem::heap_free(dp);
    mem::heap_free(res);
    mem::heap_free(exc);
    for b in lent {
        unref_disp(b);
    }
    if hr != S_OK {
        return Err(host_error(hr, scode, msg, ctx));
    }
    Ok(out)
}

/// A host call's failure as JScript shows it: an Error whose message is the EXCEPINFO's
/// description and whose number is its scode (else the HRESULT)
fn host_error(hr: u32, scode: u32, msg: String, ctx: &mut Context) -> JsError {
    let number = if hr == DISP_E_EXCEPTION && scode != 0 { scode } else { hr };
    let msg = if msg.is_empty() { format!("host call failed ({hr:#x})") } else { msg };
    let e = JsNativeError::error().with_message(msg).into_opaque(ctx);
    let _ = e.set(js_string!("number"), JsValue::new(number as i32), false, ctx);
    JsError::from_opaque(e.into())
}

/// (description, scode) of a script error for an EXCEPINFO: the Error's message (not its
/// name) and its number, if it has one
fn error_info(e: &JsError, ctx: &mut Context) -> (String, u32) {
    let number = e.as_opaque().and_then(|v| v.as_object()).and_then(|o| o.get(js_string!("number"), ctx).ok()).and_then(|n| n.as_number());
    let msg = match e.try_native(ctx) {
        Ok(n) => n.message().to_string(),
        Err(_) => e.to_string(),
    };
    (msg, number.map_or(0x800a_139e, |n| n as i32 as u32))
}

fn host_get(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let p = proxied(&args[0], ctx)?;
    let Some(name) = key_name(&args[1], ctx)? else { return Ok(JsValue::undefined()) };
    let Some((id, method)) = member(p, &name) else {
        return Ok(JsValue::undefined());
    };
    if !method {
        return host_invoke(p, id, DISPATCH_PROPERTYGET, &[], false, ctx);
    }
    let f = NativeFunction::from_copy_closure(move |_this, args, ctx| host_invoke(p, id, DISPATCH_METHOD | DISPATCH_PROPERTYGET, args, false, ctx));
    Ok(FunctionObjectBuilder::new(ctx.realm(), f).name(JsString::from(name.as_str())).length(0).build().into())
}

fn host_set(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let p = proxied(&args[0], ctx)?;
    let Some(name) = key_name(&args[1], ctx)? else { return Ok(JsValue::new(false)) };
    let Some((id, _)) = member(p, &name) else {
        return Err(throw(format!("host object has no member {name}")));
    };
    host_invoke(p, id, DISPATCH_PROPERTYPUT, &args[2..3], true, ctx)?;
    Ok(JsValue::new(true))
}

// ---------------------------------------------------------------------------------------------
// script objects for the host: IDispatch(Ex)

fn disp_vtbl() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "JScriptDispatch",
            &[
                (X_QueryInterface, 12),
                (X_AddRef, 4),
                (X_Release, 4),
                (X_GetTypeInfoCount, 8),
                (notimpl, 16),
                (X_GetIDsOfNames, 24),
                (X_Invoke, 36),
                (X_GetDispID, 16),
                (X_InvokeEx, 32),
                (notimpl, 12),
                (notimpl, 8),
                (notimpl, 16),
                (X_GetMemberName, 12),
                (notimpl, 16),
                (notimpl, 8),
            ],
        )
    })
}

fn notimpl(_: &mut Ctx) -> u32 {
    E_NOTIMPL
}

/// The IDispatch of script object `o`, with one reference for the receiver: kept by it, or
/// (an argument lent for a call) let go after the call. The block and the table's hold on the
/// object go with the last reference, so boa can collect what the host let go of.
fn script_disp(engine: u32, o: JsObject, _owned: bool) -> u32 {
    if let Some(b) = t(|t| t.disps.get(&o).copied()) {
        t(|t| t.objects.get_mut(&b).map(|x| x.2 += 1));
        return b;
    }
    let b = mem::heap_alloc(8, true);
    if b == 0 {
        return 0;
    }
    w32(b, disp_vtbl());
    w32(b + 4, engine);
    t(|t| {
        t.disps.insert(o.clone(), b);
        t.objects.insert(b, (engine, o, 1));
    });
    b
}

/// One reference of script object block `b` goes: the references left; at 0 the block is
/// freed (after com.rs's quarantine, so a late call still finds the vtable) and the object
/// is the script's alone again.
fn unref_disp(b: u32) -> u32 {
    let gone = t(|t| {
        let x = t.objects.get_mut(&b)?;
        x.2 = x.2.saturating_sub(1);
        if x.2 > 0 {
            return Some((x.2, None));
        }
        let (engine, o, _) = t.objects.remove(&b)?;
        t.disps.remove(&o);
        Some((0, Some(engine)))
    });
    match gone {
        None => 0,
        Some((n, None)) => n,
        Some((_, Some(engine))) => {
            crate::com::free_later(b);
            // (the host's last hold on the engine may have been this object)
            if depth() == 0 {
                maybe_free(engine);
            }
            0
        }
    }
}

fn name_id(name: &str) -> u32 {
    t(|t| {
        if let Some(&i) = t.name_ids.get(name) {
            return i;
        }
        t.names.push(name.to_string());
        let i = 999 + t.names.len() as u32;
        t.name_ids.insert(name.to_string(), i);
        i
    })
}
fn id_name(id: u32) -> Option<String> {
    t(|t| t.names.get(id.checked_sub(1000)? as usize).cloned())
}

fn X_QueryInterface(c: &mut Ctx) -> u32 {
    // IUnknown, IDispatch, IDispatchEx
    match r32(c.a(1)) {
        0 | 0x0002_0400 | 0xa6ef_9860 => {
            put32(c.a(2), c.a(0));
            X_AddRef(c);
            S_OK
        }
        _ => {
            put32(c.a(2), 0);
            E_NOINTERFACE
        }
    }
}
fn X_AddRef(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    t(|t| t.objects.get_mut(&b).map_or(1, |x| {
        x.2 += 1;
        x.2
    }))
}
fn X_Release(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    enter(c, || unref_disp(b))
}
fn X_GetTypeInfoCount(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    S_OK
}
fn X_GetIDsOfNames(c: &mut Ctx) -> u32 {
    let (names, n, out) = (c.a(2), c.a(3), c.a(5));
    for k in 0..n {
        let s = String::from_utf16_lossy(&mem::wstr(r32(names + 4 * k)));
        w32(out + 4 * k, name_id(&s));
    }
    S_OK
}
fn X_GetDispID(c: &mut Ctx) -> u32 {
    let s = String::from_utf16_lossy(&mem::wstr(c.a(1)));
    put32(c.a(3), name_id(&s));
    S_OK
}
fn X_GetMemberName(c: &mut Ctx) -> u32 {
    match id_name(c.a(1)) {
        Some(n) => {
            put32(c.a(2), crate::misc::bstr_alloc(&wide(&n)));
            S_OK
        }
        None => DISP_E_MEMBERNOTFOUND,
    }
}
fn X_Invoke(c: &mut Ctx) -> u32 {
    // Invoke(id, riid, lcid, flags, params, result, excep, argerr)
    let a = [c.a(1), c.a(4), c.a(5), c.a(6), c.a(7)];
    invoke_script(c, a[0], a[1], a[2], a[3], a[4])
}
fn X_InvokeEx(c: &mut Ctx) -> u32 {
    // InvokeEx(id, lcid, flags, params, result, excep, caller)
    let a = [c.a(1), c.a(3), c.a(4), c.a(5), c.a(6)];
    invoke_script(c, a[0], a[1], a[2], a[3], a[4])
}

fn invoke_script(c: &mut Ctx, id: u32, flags: u32, dp: u32, result: u32, excep: u32) -> u32 {
    let b = c.a(0);
    let Some((engine, o, _)) = t(|t| t.objects.get(&b).cloned()) else { return E_UNEXPECTED };
    let Some(ctx) = engine_ctx(engine) else { return E_UNEXPECTED };
    let (rg, n) = if dp == 0 { (0, 0) } else { (r32(dp), r32(dp + 8)) };
    if depth() >= MAX_DEPTH {
        return excepinfo_code(excep, "Out of stack space", OUT_OF_STACK);
    }
    // (the value turns into a VARIANT inside the call too: a host object in it is AddRef'd
    // through the guest, which needs the Ctx; after `enter` there was none)
    let prev = ENGINE_NOW.with(|e| e.replace(engine));
    let r = enter(c, || -> Result<Var, (String, u32)> {
        if t(|t| t.engines.get(&engine).is_some_and(|e| !e.items.is_empty())) {
            resolve_items(engine);
        }
        let args: Vec<JsValue> = (0..n).rev().map(|i| to_js(&read_var(rg + 16 * i), ctx, engine, false)).collect();
        let v = call_script(&o, id, flags, &args, ctx).map_err(|e| error_info(&e, ctx))?;
        Ok(if result != 0 { from_js(&v, engine, true) } else { Var::Empty })
    });
    ENGINE_NOW.with(|e| e.set(prev));
    match r {
        Ok(v) => {
            if result != 0 {
                write_var(result, &v);
            }
            S_OK
        }
        Err((msg, scode)) => {
            // (once, from the outermost call: a nested one's error comes back up through it)
            if depth() == 0 {
                eprintln!("sake: jscript: {msg}");
            }
            excepinfo_code(excep, &msg, scode)
        }
    }
}

/// DISP_E_EXCEPTION with `msg` and `scode` in the caller's EXCEPINFO (if it gave one)
fn excepinfo_code(excep: u32, msg: &str, scode: u32) -> u32 {
    if excep != 0 {
        mem::fill(excep, 32, 0);
        w32(excep + 8, crate::misc::bstr_alloc(&wide(msg)));
        w32(excep + 28, scode);
    }
    DISP_E_EXCEPTION
}

/// Member `id` of script object `o` (0: the object itself) read, written or called
fn call_script(o: &JsObject, id: u32, flags: u32, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    if id == 0 {
        if flags & DISPATCH_METHOD != 0 && o.is_callable() {
            return o.call(&JsValue::undefined(), args, ctx);
        }
        return Ok(o.clone().into());
    }
    let Some(name) = id_name(id) else { return Err(throw("unknown member".into())) };
    let key = JsString::from(name.as_str());
    if flags & (DISPATCH_PROPERTYPUT | 8) != 0 {
        o.set(key, args.last().cloned().unwrap_or_default(), true, ctx)?;
        return Ok(JsValue::undefined());
    }
    let v = o.get(key, ctx)?;
    if flags & DISPATCH_METHOD != 0 {
        if let Some(f) = v.as_callable() {
            return f.call(&o.clone().into(), args, ctx);
        }
        if flags & DISPATCH_PROPERTYGET == 0 {
            return Err(throw(format!("{name} is not a function")));
        }
    }
    Ok(v)
}

// ---------------------------------------------------------------------------------------------
// the engine

thread_local! {
    static ENGINE_NOW: Cell<u32> = const { Cell::new(0) };
}
fn current_engine() -> u32 {
    ENGINE_NOW.with(|e| e.get())
}

fn engine_vtbls() -> [u32; 4] {
    static V: std::sync::OnceLock<[u32; 4]> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        [
            vtable(
                "IActiveScript",
                &[
                    (Q_QueryInterface, 12),
                    (Q_AddRef, 4),
                    (Q_Release, 4),
                    (SetScriptSite, 8),
                    (GetScriptSite, 12),
                    (SetScriptState, 8),
                    (GetScriptState, 8),
                    (Close, 4),
                    (AddNamedItem, 12),
                    (ok, 20),
                    (GetScriptDispatch, 12),
                    (GetCurrentScriptThreadID, 8),
                    (GetScriptThreadID, 12),
                    (GetScriptThreadState, 12),
                    (ok, 16),
                    (notimpl, 8),
                ],
            ),
            vtable(
                "IActiveScriptParse32",
                &[(Q_QueryInterface, 12), (Q_AddRef, 4), (Q_Release, 4), (ok, 4), (notimpl, 48), (ParseScriptText, 40)],
            ),
            vtable("IObjectSafety", &[(Q_QueryInterface, 12), (Q_AddRef, 4), (Q_Release, 4), (GetInterfaceSafetyOptions, 16), (ok, 16)]),
            vtable("IActiveScriptProperty", &[(Q_QueryInterface, 12), (Q_AddRef, 4), (Q_Release, 4), (GetProperty, 16), (ok, 16)]),
        ]
    })
}

fn ok(_: &mut Ctx) -> u32 {
    S_OK
}

fn engine_of(c: &Ctx) -> u32 {
    r32(c.a(0) + 4)
}

/// CoCreateInstance of JScript ({f414c260-...}) or JScript9 ({16d51579-...}).
pub fn create_instance(clsid: u32) -> Option<u32> {
    match r32(clsid) {
        0xf414_c260 | 0x16d5_1579 | 0xf414_c262 => {}
        _ => return None,
    }
    let mut ctx = Box::new(Context::default());
    ctx.runtime_limits_mut().set_recursion_limit(20_000);
    ctx.runtime_limits_mut().set_stack_size_limit(1 << 20);
    jscript_globals(&mut ctx);
    let vt = engine_vtbls();
    let id = t(|t| {
        t.next += 1;
        t.next
    });
    let blocks = vt.map(|v| {
        let b = mem::heap_alloc(8, true);
        w32(b, v);
        w32(b + 4, id);
        b
    });
    t(|t| {
        t.engines.insert(id, Engine { ctx: Box::into_raw(ctx), site: 0, state: 0, items: Vec::new(), pending: Vec::new(), blocks, refs: 1 })
    });
    Some(blocks[0])
}

/// What JScript has beyond the language: CollectGarbage, ScriptEngine*.
fn jscript_globals(ctx: &mut Context) {
    let f = |v: JsValue| NativeFunction::from_copy_closure_with_captures(|_, _, v: &JsValue, _| Ok(v.clone()), v);
    let _ = ctx.register_global_callable(js_string!("CollectGarbage"), 0, f(JsValue::undefined()));
    let _ = ctx.register_global_callable(js_string!("ScriptEngine"), 0, f(JsValue::new(js_string!("JScript"))));
    let _ = ctx.register_global_callable(js_string!("ScriptEngineMajorVersion"), 0, f(JsValue::new(9)));
    let _ = ctx.register_global_callable(js_string!("ScriptEngineMinorVersion"), 0, f(JsValue::new(0)));
    let _ = ctx.register_global_callable(js_string!("ScriptEngineBuildVersion"), 0, f(JsValue::new(16384)));
}

fn Q_QueryInterface(c: &mut Ctx) -> u32 {
    let id = engine_of(c);
    let Some(blocks) = t(|t| t.engines.get(&id).map(|e| e.blocks)) else { return E_UNEXPECTED };
    let k = match r32(c.a(1)) {
        0 | 0xbb1a_2ae1 => 0,
        0xbb1a_2ae2 => 1,
        0xcb5b_dc81 => 2,
        0x4954_e0d0 => 3,
        g => {
            crate::com::warn_once(&format!("sake: jscript: QueryInterface {g:08x}: no"));
            put32(c.a(2), 0);
            return E_NOINTERFACE;
        }
    };
    put32(c.a(2), blocks[k]);
    Q_AddRef(c);
    S_OK
}
fn Q_AddRef(c: &mut Ctx) -> u32 {
    let id = engine_of(c);
    t(|t| t.engines.get_mut(&id).map_or(1, |e| {
        e.refs += 1;
        e.refs
    }))
}
fn Q_Release(c: &mut Ctx) -> u32 {
    let id = engine_of(c);
    let n = t(|t| t.engines.get_mut(&id).map_or(0, |e| {
        e.refs = e.refs.saturating_sub(1);
        e.refs
    }));
    if n == 0 {
        // (outside any script call: freed as the call returns; inside one, when the
        // outermost call ends)
        enter(c, || {});
    }
    n
}

/// Frees engine `id` once nothing holds it: no reference to its interfaces, no script object
/// of it the host still holds, no script of it running. Its proxies let their host objects go,
/// its realm (boa's Context, about a MB) and its blocks are freed. A program that made an engine
/// per script kept every one of them.
fn maybe_free(id: u32) {
    if depth() != 0 {
        return;
    }
    let free = t(|t| t.engines.get(&id).is_some_and(|e| e.refs == 0) && !t.objects.values().any(|x| x.0 == id));
    if !free {
        return;
    }
    let Some(e) = t(|t| t.engines.remove(&id)) else { return };
    let hosts: Vec<(u32, JsObject)> = t(|t| {
        let keys: Vec<(u32, u32)> = t.proxies.keys().filter(|k| k.0 == id).copied().collect();
        keys.into_iter().filter_map(|k| t.proxies.remove(&k).map(|o| (k.1, o))).collect()
    });
    for (p, o) in hosts {
        t(|t| t.hosts.remove(&o));
        drop(o);
        // (a guest call: only where a Ctx is current; otherwise the reference stays)
        if !CUR.with(|c| c.get()).is_null() {
            com(p, 2, &[]);
        }
    }
    if e.site != 0 && !CUR.with(|c| c.get()).is_null() {
        com(e.site, 2, &[]);
    }
    // SAFETY: the Context was Box::into_raw'd by create_instance, nothing runs in it (depth 0)
    // and no table refers to it any more
    drop(unsafe { Box::from_raw(e.ctx) });
    for b in e.blocks {
        crate::com::free_later(b);
    }
}

fn SetScriptSite(c: &mut Ctx) -> u32 {
    let (id, site) = (engine_of(c), c.a(1));
    if site != 0 {
        enter(c, || com(site, 1, &[]));
    }
    t(|t| t.engines.get_mut(&id).map(|e| {
        e.site = site;
        if e.state == 0 {
            e.state = INITIALIZED;
        }
    }));
    S_OK
}
fn GetScriptSite(c: &mut Ctx) -> u32 {
    let id = engine_of(c);
    let site = t(|t| t.engines.get(&id).map_or(0, |e| e.site));
    if site == 0 {
        put32(c.a(2), 0);
        return S_FALSE;
    }
    let (riid, out) = (c.a(1), c.a(2));
    enter(c, || com(site, 0, &[riid, out]))
}
fn GetScriptState(c: &mut Ctx) -> u32 {
    let id = engine_of(c);
    put32(c.a(1), t(|t| t.engines.get(&id).map_or(0, |e| e.state)));
    S_OK
}
fn SetScriptState(c: &mut Ctx) -> u32 {
    let (id, state) = (engine_of(c), c.a(1));
    let pending = t(|t| {
        t.engines.get_mut(&id).map(|e| {
            e.state = state;
            if state == STARTED || state == CONNECTED { std::mem::take(&mut e.pending) } else { Vec::new() }
        })
    });
    let Some(pending) = pending else { return E_UNEXPECTED };
    let site = t(|t| t.engines.get(&id).map_or(0, |e| e.site));
    enter(c, || {
        if site != 0 {
            com(site, 7, &[state]);
        }
        for code in pending {
            run(id, &code, false).ok();
        }
    });
    S_OK
}
fn Close(c: &mut Ctx) -> u32 {
    let id = engine_of(c);
    let site = t(|t| {
        t.engines.get_mut(&id).map_or(0, |e| {
            e.state = CLOSED;
            std::mem::take(&mut e.site)
        })
    });
    if site != 0 {
        enter(c, || com(site, 2, &[]));
    }
    S_OK
}
fn AddNamedItem(c: &mut Ctx) -> u32 {
    let (id, name, flags) = (engine_of(c), mem::wstr(c.a(1)), c.a(2));
    t(|t| t.engines.get_mut(&id).map(|e| e.items.push((name, flags))));
    enter(c, || resolve_items(id));
    S_OK
}

/// Makes the named items the site can give globals.
fn resolve_items(id: u32) {
    let Some((site, items)) = t(|t| t.engines.get_mut(&id).map(|e| (e.site, std::mem::take(&mut e.items)))) else { return };
    if site == 0 {
        t(|t| t.engines.get_mut(&id).map(|e| e.items = items));
        return;
    }
    let Some(ctx) = engine_ctx(id) else { return };
    for (name, flags) in items {
        let s = mem::heap_alloc(2 * name.len() as u32 + 2, true);
        for (i, &ch) in name.iter().enumerate() {
            w16(s + 2 * i as u32, ch);
        }
        let out = mem::heap_alloc(8, true);
        // GetItemInfo(name, SCRIPTINFO_IUNKNOWN, &unk, NULL)
        let hr = com(site, 4, &[s, 1, out, 0]);
        let unk = r32(out);
        mem::heap_free(s);
        if hr != S_OK || unk == 0 {
            // (the host may not have it yet: asked again before the next script runs)
            crate::com::warn_once(&format!("sake: jscript: no object for named item {} yet", String::from_utf16_lossy(&name)));
            mem::heap_free(out);
            t(|t| t.engines.get_mut(&id).map(|e| e.items.push((name, flags))));
            continue;
        }
        // its IDispatch {00020400-0000-0000-c000-000000000046}
        let iid = mem::heap_alloc(16, true);
        mem::write(iid, &[0, 4, 2, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
        let qi = com(unk, 0, &[iid, out]) == S_OK && r32(out) != 0;
        let disp = if qi { r32(out) } else { unk };
        mem::heap_free(iid);
        mem::heap_free(out);
        // (GetItemInfo's IUnknown is ours too: the IDispatch QueryInterface gave is kept,
        // and it is a reference of its own even when it is the same pointer)
        if qi {
            com(unk, 2, &[]);
        }
        if flags & 8 != 0 {
            crate::com::warn_once("sake: jscript: SCRIPTITEM_GLOBALMEMBERS is not supported");
        }
        if let Some(p) = host_proxy(id, disp, ctx, true) {
            let key = JsString::from(&name[..]);
            let _ = ctx.global_object().define_property_or_throw(key, PropertyDescriptor::builder().value(p).writable(true).enumerable(true).configurable(true), ctx);
        }
    }
}

/// Runs script text in the engine's global scope; errors go to the site.
fn run(id: u32, code: &[u16], expr: bool) -> Result<JsValue, u32> {
    resolve_items(id);
    let Some(ctx) = engine_ctx(id) else { return Err(E_UNEXPECTED) };
    let prev = ENGINE_NOW.with(|e| e.replace(id));
    let r = ctx.eval(Source::from_utf16(code));
    let _ = ctx.run_jobs();
    ENGINE_NOW.with(|e| e.set(prev));
    match r {
        Ok(v) => Ok(if expr { v } else { JsValue::undefined() }),
        Err(e) => {
            let msg = e.to_string();
            eprintln!("sake: jscript error: {msg}");
            report(id, &msg);
            Err(SCRIPT_E_REPORTED)
        }
    }
}

/// IActiveScriptError for the site's OnScriptError.
fn report(id: u32, msg: &str) {
    let site = t(|t| t.engines.get(&id).map_or(0, |e| e.site));
    if site == 0 {
        return;
    }
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *V.get_or_init(|| {
        vtable("IActiveScriptError", &[(E_QueryInterface, 12), (E_AddRef, 4), (E_Release, 4), (E_GetExceptionInfo, 8), (E_GetSourcePosition, 16), (E_GetSourceLineText, 8)])
    });
    let w = wide(msg);
    // { vtable, length, references, text }: the site may keep it (AddRef); it goes with its
    // last Release (it stayed, with its text, per error)
    let e = mem::heap_alloc(12 + 2 * w.len() as u32 + 2, true);
    if e == 0 {
        return;
    }
    w32(e, vt);
    w32(e + 4, w.len() as u32);
    w32(e + 8, 1);
    for (i, &ch) in w.iter().enumerate() {
        w16(e + 12 + 2 * i as u32, ch);
    }
    com(site, 8, &[e]);
    error_release(e);
}
fn E_AddRef(c: &mut Ctx) -> u32 {
    let e = c.a(0);
    w32(e + 8, r32(e + 8) + 1);
    r32(e + 8)
}
fn E_Release(c: &mut Ctx) -> u32 {
    error_release(c.a(0))
}
fn error_release(e: u32) -> u32 {
    let n = r32(e + 8).saturating_sub(1);
    w32(e + 8, n);
    if n == 0 {
        crate::com::free_later(e);
    }
    n
}
fn E_QueryInterface(c: &mut Ctx) -> u32 {
    put32(c.a(2), c.a(0));
    E_AddRef(c);
    S_OK
}
fn E_GetExceptionInfo(c: &mut Ctx) -> u32 {
    let (e, p) = (c.a(0), c.a(1));
    let n = r32(e + 4);
    let msg: Vec<u16> = (0..n).map(|i| r16(e + 12 + 2 * i)).collect();
    mem::fill(p, 32, 0);
    w32(p + 4, crate::misc::bstr_alloc(&wide("Microsoft JScript runtime error")));
    w32(p + 8, crate::misc::bstr_alloc(&msg));
    w32(p + 28, 0x800a_139e);
    S_OK
}
fn E_GetSourcePosition(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    put32(c.a(2), 0);
    put32(c.a(3), 0);
    S_OK
}
fn E_GetSourceLineText(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    E_NOTIMPL
}

/// ParseScriptText(code, item, context, delimiter, cookie, line, flags, result, excep)
fn ParseScriptText(c: &mut Ctx) -> u32 {
    let id = engine_of(c);
    let (code, flags, result) = (mem::wstr(c.a(1)), c.a(7), c.a(8));
    let expr = flags & 0x20 != 0;
    let state = t(|t| t.engines.get(&id).map_or(0, |e| e.state));
    if !expr && (state == INITIALIZED || state == 0) {
        t(|t| t.engines.get_mut(&id).map(|e| e.pending.push(code)));
        return S_OK;
    }
    // (the result becomes a VARIANT inside the call: a host object in it is AddRef'd through
    // the guest, which needs the Ctx)
    match enter(c, || run(id, &code, expr).map(|v| if result != 0 { from_js(&v, id, true) } else { Var::Empty })) {
        Ok(var) => {
            if result != 0 {
                write_var(result, &var);
            }
            S_OK
        }
        Err(e) => e,
    }
}

/// GetScriptDispatch(item, &disp): the global object (or a named item's object).
fn GetScriptDispatch(c: &mut Ctx) -> u32 {
    let id = engine_of(c);
    let Some(ctx) = engine_ctx(id) else { return E_UNEXPECTED };
    if c.a(1) != 0 {
        let name = JsString::from(&mem::wstr(c.a(1))[..]);
        let g = ctx.global_object();
        if let Some(p) = g.get(name, ctx).ok().and_then(|v| v.as_object()).and_then(|o| t(|t| t.hosts.get(&o).copied())) {
            enter(c, || com(p, 1, &[]));
            mem::putp(c.a(2), p);
            return S_OK;
        }
        put32(c.a(2), 0);
        return E_INVALIDARG;
    }
    put32(c.a(2), script_disp(id, ctx.global_object(), true));
    S_OK
}
fn GetCurrentScriptThreadID(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    S_OK
}
fn GetScriptThreadID(c: &mut Ctx) -> u32 {
    put32(c.a(2), 0);
    S_OK
}
fn GetScriptThreadState(c: &mut Ctx) -> u32 {
    put32(c.a(2), 0);
    S_OK
}
fn GetInterfaceSafetyOptions(c: &mut Ctx) -> u32 {
    put32(c.a(2), 3);
    put32(c.a(3), 0);
    S_OK
}
fn GetProperty(c: &mut Ctx) -> u32 {
    if c.a(3) != 0 {
        write_var(c.a(3), &Var::Empty);
    }
    S_OK
}

/// CLSIDFromProgID for the script engines' names.
pub fn progid(name: &str) -> Option<[u8; 16]> {
    match name.to_ascii_lowercase().as_str() {
        "jscript" | "javascript" | "ecmascript" | "livescript" | "jscript.encode" => {
            Some([0x60, 0xc2, 0x14, 0xf4, 0xc0, 0x6a, 0xcf, 0x11, 0xb6, 0xd1, 0x00, 0xaa, 0x00, 0xbb, 0xbb, 0x58])
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_to_variants() {
        assert_eq!(primitive(&JsValue::new(3)), Some(Var::I4(3)));
        assert_eq!(primitive(&JsValue::new(3.5)), Some(Var::R8(3.5)));
        assert_eq!(primitive(&JsValue::new(4_000_000_000.0)), Some(Var::R8(4e9)));
        assert_eq!(primitive(&JsValue::new(-0.0)), Some(Var::R8(-0.0)));
        assert_eq!(primitive(&JsValue::undefined()), Some(Var::Empty));
        assert_eq!(primitive(&JsValue::new(js_string!("a"))), Some(Var::Str(vec![97])));
    }

    /// JScript code SRPG Studio style scripts rely on: ES5 objects and closures, the JScript
    /// globals.
    #[test]
    fn runs_es5() {
        let mut ctx = Context::default();
        jscript_globals(&mut ctx);
        let v = ctx
            .eval(Source::from_bytes(
                "var Base = { f: function(x) { return x * 2; } };\
                 var D = Object.create(Base); D.g = function() { return this.f(21); };\
                 CollectGarbage(); ScriptEngine() + ':' + D.g()",
            ))
            .unwrap();
        assert_eq!(v.as_string().unwrap().to_std_string_escaped(), "JScript:42");
    }
}
