//! Deferred contexts through the real entry point (`d3d11::call`), on the cases of Wine's
//! d3d11 test_deferred_context_state / _map / _queries and Windows' rules: a deferred context's
//! own state, what FinishCommandList keeps or clears, what ExecuteCommandList gives back,
//! Map's limits on a deferred context and the data its Unmap records, and references.

use super::*;
use crate::d3d9::trace_tests::{alloc, CALLS, SERIAL};

fn mem(n: u32) -> u32 {
    unsafe { alloc(4 * n) as u32 }
}
fn put(p: u32, v: &[u32]) {
    for (i, &x) in v.iter().enumerate() {
        unsafe { *((p + 4 * i as u32) as *mut u32) = x };
    }
}
fn rd1(p: u32) -> u32 {
    unsafe { *(p as *const u32) }
}
fn call1(id: u32, args: &[u32]) -> u32 {
    unsafe { call(id, args.as_ptr(), &CALLS) }
}
fn m(iface: u32, m: u32) -> u32 {
    iface << 8 | m
}
fn iface_of(a: u32) -> u32 {
    let g = D11S.lock().unwrap();
    let d = g.as_ref().unwrap();
    d.objs[&d.id_of(a)].faces.iter().find(|f| f.1 == a).unwrap().0
}
fn release(a: u32) -> u32 {
    call1(m(iface_of(a), 2), &[a])
}
fn refs(a: u32) -> u32 {
    call1(m(iface_of(a), 1), &[a]);
    release(a)
}
/// A buffer's contents as dwords.
fn contents(a: u32, n: usize) -> Vec<u32> {
    let g = D11S.lock().unwrap();
    let d = g.as_ref().unwrap();
    let Data::Buffer(b) = &d.objs[&d.id_of(a)].data else { panic!("not a buffer") };
    (0..n).map(|i| unsafe { *((b.mem + 4 * i as u32) as *const u32) }).collect()
}

struct Ctx {
    dev: u32,
    imm: u32,
    o: u32,
}

impl Ctx {
    fn new() -> Ctx {
        let (dev_out, level, ctx_out) = (mem(2), mem(2), mem(2));
        assert_eq!(call1(CREATE_DEVICE, &[0, 1, 0, 0, 0, 0, 7, 0, 0, dev_out, level, ctx_out]), 0);
        Ctx { dev: rd1(dev_out), imm: rd1(ctx_out), o: mem(2) }
    }
    fn buffer(&self, size: u32, usage: u32, bind: u32, cpu: u32, data: Option<&[u32]>) -> u32 {
        let d = mem(6);
        put(d, &[size, usage, bind, cpu, 0, 0]);
        let init = data.map_or(0, |v| {
            let p = mem(v.len() as u32);
            put(p, v);
            let s = mem(3);
            put(s, &[p, 0, 0]);
            s
        });
        assert_eq!(call1(m(IF_DEVICE, 3), &[self.dev, d, init, self.o]), 0);
        rd1(self.o)
    }
    fn deferred(&self) -> u32 {
        assert_eq!(call1(m(IF_DEVICE, 27), &[self.dev, 0, self.o]), 0);
        rd1(self.o)
    }
    fn set_ps_cb(&self, ctx: u32, b: u32) {
        let p = mem(2);
        put(p, &[b, 0]);
        call1(m(IF_CONTEXT, 16), &[ctx, 0, 1, p]);
    }
    /// PSGetConstantBuffers slot 0 (the reference it gives dropped)
    fn ps_cb(&self, ctx: u32) -> u32 {
        let p = mem(2);
        put(p, &[0xdead, 0]);
        call1(m(IF_CONTEXT, 77), &[ctx, 0, 1, p]);
        let b = rd1(p);
        if b != 0 {
            release(b);
        }
        b
    }
    fn finish(&self, ctx: u32, restore: u32) -> u32 {
        put(self.o, &[0xdead]);
        assert_eq!(call1(m(IF_CONTEXT, 114), &[ctx, restore, self.o]), 0);
        rd1(self.o)
    }
    fn execute(&self, ctx: u32, list: u32, restore: u32) {
        call1(m(IF_CONTEXT, 58), &[ctx, list, restore]);
    }
    /// Map(ctx, res, 0, type): (hr, D3D11_MAPPED_SUBRESOURCE)
    fn map(&self, ctx: u32, res: u32, ty: u32) -> (u32, [u32; 3]) {
        let out = mem(3);
        put(out, &[0xdeadbeef, 0xabab, 0xcdcd]);
        let hr = call1(m(IF_CONTEXT, 14), &[ctx, res, 0, ty, 0, out]);
        (hr, [rd1(out), rd1(out + 4), rd1(out + 8)])
    }
    fn unmap(&self, ctx: u32, res: u32) {
        call1(m(IF_CONTEXT, 15), &[ctx, res, 0]);
    }
}

fn setup() -> std::sync::MutexGuard<'static, ()> {
    let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let vt = mem(256);
    put(vt, &(1..=256).map(|i| 0x1000 * i).collect::<Vec<_>>());
    call1(INIT, &[vt, 256]);
    g
}

#[test]
fn deferred_state() {
    let _g = setup();
    let c = Ctx::new();
    let dev_refs = refs(c.dev);
    let green = c.buffer(16, 0, 4, 0, Some(&[0, 0x3f80_0000, 0, 0x3f80_0000]));
    let blue = c.buffer(16, 0, 4, 0, Some(&[0, 0, 0x3f80_0000, 0x3f80_0000]));
    c.set_ps_cb(c.imm, green);
    let def = c.deferred();
    // a device child holding the device; its own (default) state
    assert_eq!(refs(def), 1);
    assert_eq!(refs(c.dev), dev_refs + 3);
    assert_eq!(call1(m(IF_CONTEXT, 112), &[def]), 1);
    assert_eq!(call1(m(IF_CONTEXT, 112), &[c.imm]), 0);
    assert_eq!(c.ps_cb(def), 0);
    c.set_ps_cb(def, blue);
    assert_eq!(c.ps_cb(def), blue);
    assert_eq!(c.ps_cb(c.imm), green);
    // FinishCommandList(TRUE) keeps the state, (FALSE) clears it
    let list1 = c.finish(def, 1);
    assert_eq!(c.ps_cb(def), blue);
    let list2 = c.finish(def, 0);
    assert_eq!(c.ps_cb(def), 0);
    // ExecuteCommandList: the immediate state back (TRUE) or cleared (FALSE)
    c.set_ps_cb(c.imm, green);
    c.execute(c.imm, list1, 1);
    assert_eq!(c.ps_cb(c.imm), green);
    c.execute(c.imm, list1, 0);
    assert_eq!(c.ps_cb(c.imm), 0);
    // (list2 started from list1's end state: blue, and changed nothing)
    c.set_ps_cb(c.imm, green);
    assert_eq!(release(list2), 0);
    assert_eq!(release(list1), 0);
    // a list executed on another deferred context
    let def2 = c.deferred();
    c.set_ps_cb(def2, green);
    let list = c.finish(def2, 0);
    c.set_ps_cb(def, blue);
    c.execute(def, list, 1);
    assert_eq!(c.ps_cb(def), blue);
    c.execute(def, list, 0);
    assert_eq!(c.ps_cb(def), 0);
    // ... and that list replayed: green, then cleared
    let outer = c.finish(def, 0);
    c.set_ps_cb(c.imm, blue);
    c.execute(c.imm, outer, 1);
    assert_eq!(c.ps_cb(c.imm), blue);
    assert_eq!(release(outer), 0);
    assert_eq!(release(list), 0);
    assert_eq!(release(def2), 0);
    // a deferred context is no ID3D11Multithread, nor does it give query data
    let iid = mem(4);
    put(iid, &[0x9b7e4e00, 0, 0, 0]);
    assert_eq!(call1(m(IF_CONTEXT, 0), &[def, iid, c.o]), E_NOINTERFACE);
    assert_eq!(release(def), 0);
    c.set_ps_cb(c.imm, 0);
    assert_eq!(release(green), 0);
    assert_eq!(release(blue), 0);
    assert_eq!(refs(c.dev), dev_refs);
    release(c.imm);
    release(c.dev);
}

#[test]
fn deferred_map() {
    let _g = setup();
    let c = Ctx::new();
    let data: Vec<u32> = (0..16).map(|i| (i as f32).to_bits()).collect();
    let f = |k: u32| -> Vec<u32> { (0..16).map(|i| ((k * i) as f32).to_bits()).collect() };
    let buf = c.buffer(64, 2, 1, 0x10000, Some(&data));
    let def = c.deferred();
    // only WRITE_DISCARD first; a failed map clears pData and leaves the pitches
    for ty in [1, 3, 2] {
        assert_eq!(c.map(def, buf, ty), (E_INVALIDARG, [0, 0xabab, 0xcdcd]), "type {ty}");
    }
    assert_eq!(c.map(def, buf, 5).0, 0x887c_0004);
    let (hr, [p, ..]) = c.map(def, buf, 4);
    assert_eq!(hr, 0);
    put(p, &f(2));
    c.unmap(def, buf);
    let (hr, [p, ..]) = c.map(def, buf, 4);
    assert_eq!(hr, 0);
    put(p, &f(3));
    c.unmap(def, buf);
    // NO_OVERWRITE after a discard in the same list: the same memory
    let (hr, [p2, ..]) = c.map(def, buf, 5);
    assert_eq!((hr, p2), (0, p));
    c.unmap(def, buf);
    let list = c.finish(def, 0);
    assert_eq!(contents(buf, 16), data);
    c.execute(c.imm, list, 1);
    assert_eq!(contents(buf, 16), f(3));
    assert_eq!(release(list), 0);
    // an unmap on the wrong context changes nothing (Dead Rising does it)
    let (hr, [p, ..]) = c.map(c.imm, buf, 4);
    assert_eq!(hr, 0);
    c.unmap(def, buf);
    put(p, &f(5));
    c.unmap(c.imm, buf);
    assert_eq!(contents(buf, 16), f(5));
    // a default buffer is not mapped on a deferred context
    let def_buf = c.buffer(64, 0, 1, 0, Some(&data));
    assert_eq!(c.map(def, def_buf, 4).0, E_INVALIDARG);
    assert_eq!(release(def_buf), 0);
    assert_eq!(release(def), 0);
    assert_eq!(release(buf), 0);
    release(c.imm);
    release(c.dev);
}

#[test]
fn deferred_queries() {
    let _g = setup();
    let c = Ctx::new();
    let def = c.deferred();
    // (D3D11_QUERY_OCCLUSION)
    let qd = mem(2);
    put(qd, &[2, 0]);
    assert_eq!(call1(m(IF_DEVICE, 24), &[c.dev, qd, c.o]), 0);
    let q = rd1(c.o);
    let data = mem(4);
    let get = |ctx: u32| call1(m(IF_CONTEXT, 29), &[ctx, q, data, 8, 0]);
    assert_eq!(call1(m(IF_CONTEXT, 29), &[def, q, 0, 0, 0]), DXGI_ERROR_INVALID_CALL);
    call1(m(IF_CONTEXT, 27), &[def, q]);
    assert_eq!(get(def), DXGI_ERROR_INVALID_CALL);
    call1(m(IF_CONTEXT, 28), &[def, q]);
    assert_eq!(get(def), DXGI_ERROR_INVALID_CALL);
    // (recorded only: not ended on the immediate context yet)
    assert_eq!(get(c.imm), DXGI_ERROR_INVALID_CALL);
    let list = c.finish(def, 0);
    assert_eq!(get(c.imm), DXGI_ERROR_INVALID_CALL);
    c.execute(c.imm, list, 0);
    assert_eq!(get(def), DXGI_ERROR_INVALID_CALL);
    assert_eq!(get(c.imm), 0);
    // the list holds the query: it outlives the application's reference
    assert_eq!(release(q), 0);
    assert_eq!(release(list), 0);
    assert_eq!(release(def), 0);
    release(c.imm);
    release(c.dev);
}
