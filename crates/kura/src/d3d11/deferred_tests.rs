//! Deferred contexts through the real entry point (`d3d11::call`), on the cases of Wine's
//! d3d11 test_deferred_context_state / _map / _queries and Windows' rules: a deferred context's
//! own state, what FinishCommandList keeps or clears, what ExecuteCommandList gives back,
//! Map's limits on a deferred context and the data its Unmap records, and references.

use super::*;
use crate::d3d9::trace_tests::{alloc, client_rect, free, CALLS, SERIAL};

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
    // (a fresh device as in a process of its own: what other tests left on the one device —
    // objects still bound, references — would count in these tests' reference counts)
    *D11S.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
    // both contexts are an ID3DUserDefinedAnnotation (Wine's test_user_defined_annotation)
    put(iid, &[0xb2daad8b, 0x4dbf03d4, 0xab32eb95, 0xabd0634b]);
    for ctx in [c.imm, def] {
        assert_eq!(call1(m(IF_CONTEXT, 0), &[ctx, iid, c.o]), 0);
        let an = rd1(c.o);
        let name = mem(4);
        assert_eq!(call1(m(IF_ANNOTATION, 3), &[an, name]), 0xffff_ffff);
        assert_eq!(call1(m(IF_ANNOTATION, 4), &[an]), 0xffff_ffff);
        call1(m(IF_ANNOTATION, 5), &[an, name]);
        assert_eq!(call1(m(IF_ANNOTATION, 6), &[an]), 0);
        release(an);
    }
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

fn recorded(ctx: u32) -> (usize, usize) {
    let g = D11S.lock().unwrap();
    let d = g.as_ref().unwrap();
    deferred::recorded(d, d.id_of(ctx))
}

/// UpdateSubresource on a deferred context: what it reads is copied (no more: a box and the
/// pitches decide it), sizes that do not fit are not recorded, and a list runs as often as it
/// is executed with the same data.
#[test]
fn deferred_update_sizes() {
    let _g = setup();
    let c = Ctx::new();
    let data: Vec<u32> = (0..16).collect();
    let buf = c.buffer(64, 0, 1, 0, Some(&vec![0; 16]));
    let def = c.deferred();
    let src = mem(16);
    put(src, &data);
    // a box of bytes 8..24 of the buffer: 16 bytes from `src`
    let bx = mem(6);
    put(bx, &[8, 0, 0, 24, 1, 1]);
    call1(m(IF_CONTEXT, 48), &[def, buf, 0, bx, src, 0, 0]);
    // (a box past the end reads up to it)
    let bx2 = mem(6);
    put(bx2, &[56, 0, 0, 1000, 1, 1]);
    call1(m(IF_CONTEXT, 48), &[def, buf, 0, bx2, src, 0, 0]);
    assert_eq!(recorded(def), (2, 24 + 16 + 24 + 8));
    // the source changes after recording: the list has its own copy
    put(src, &[0xffff_ffff; 16]);
    let list = c.finish(def, 0);
    for _ in 0..2 {
        c.execute(c.imm, list, 1);
        let mut want = vec![0u32; 16];
        want[2..6].copy_from_slice(&data[..4]);
        want[14..16].copy_from_slice(&data[..2]);
        assert_eq!(contents(buf, 16), want);
    }
    assert_eq!(release(list), 0);
    // a texture: pitches the application gives that make the size overflow are not recorded
    let td = mem(11);
    put(td, &[64, 64, 1, 1, 28, 1, 0, 0, 0x8, 0, 0]);
    assert_eq!(call1(m(IF_DEVICE, 5), &[c.dev, td, 0, c.o]), 0);
    let tex = rd1(c.o);
    let tb = mem(6);
    put(tb, &[0, 0, 0, 64, 64, 1]);
    for pitch in [0x8000_0000u32, 0xffff_ffff, 0x0400_0000] {
        call1(m(IF_CONTEXT, 48), &[def, tex, 0, tb, src, pitch, 0]);
    }
    assert_eq!(recorded(def), (0, 0));
    // ... and one that fits: 63 rows of 256 + one row of 64 texels of 4 bytes
    let big = unsafe { alloc(63 * 256 + 256) } as u32;
    call1(m(IF_CONTEXT, 48), &[def, tex, 0, tb, big, 256, 0]);
    assert_eq!(recorded(def), (1, 24 + 63 * 256 + 256));
    let list = c.finish(def, 0);
    c.execute(c.imm, list, 0);
    assert_eq!(release(list), 0);
    // Map of a 2048^3 3D texture on a deferred context: too large, not a wrapped size
    let d3 = mem(10);
    put(d3, &[2048, 2048, 2048, 1, 2, 2, 0x8, 0x10000, 0]);
    if call1(m(IF_DEVICE, 6), &[c.dev, d3, 0, c.o]) == 0 {
        let t3 = rd1(c.o);
        let (hr, [p, ..]) = c.map(def, t3, 4);
        assert_eq!((hr, p), (E_OUTOFMEMORY, 0));
        assert_eq!(release(t3), 0);
    }
    assert_eq!(release(tex), 0);
    assert_eq!(release(def), 0);
    assert_eq!(release(buf), 0);
    release(c.imm);
    release(c.dev);
}

/// ID3D11Device3/4/5 and ID3D11DeviceContext3/4 (Windows 10's, which AdvHD's engine asks for
/// right after D3D11CreateDevice and uses unchecked): the same objects answer, the newer
/// creation methods make what the old ones make.
#[test]
fn newer_interfaces() {
    let _g = setup();
    let c = Ctx::new();
    let guid = |d: [u32; 4]| {
        let p = mem(4);
        put(p, &d);
        p
    };
    let dev3 = guid([0xa05c8c37, 0x4732d2c6, 0xa342a0b3, 0xc59a2aa5]);
    let dev4 = guid([0x8992ab71, 0x4b8d02e6, 0xb048ba48, 0xc442dadc]);
    let dev5 = guid([0x8ffde202, 0x45dfa0e7, 0x37e8019e, 0xa05e1b80]);
    let ctx3 = guid([0xb4e3c01d, 0x4637e79e, 0x0e51b291, 0x8f9b4c9f]);
    let ctx4 = guid([0x917600da, 0x4c33f58c, 0x153ed898, 0x24fa90b3]);
    for iid in [dev3, dev4, dev5] {
        assert_eq!(call1(m(IF_DEVICE, 0), &[c.dev, iid, c.o]), 0);
        assert_eq!(rd1(c.o), c.dev);
        release(c.dev);
    }
    for iid in [ctx3, ctx4] {
        assert_eq!(call1(m(IF_CONTEXT, 0), &[c.imm, iid, c.o]), 0);
        assert_eq!(rd1(c.o), c.imm);
        release(c.imm);
    }
    // GetImmediateContext3: the immediate context, referenced
    call1(m(IF_DEVICE, 61), &[c.dev, c.o]);
    assert_eq!(rd1(c.o), c.imm);
    release(c.imm);
    // CreateDeferredContext3
    assert_eq!(call1(m(IF_DEVICE, 62), &[c.dev, 0, c.o]), 0);
    let def = rd1(c.o);
    assert_ne!(def, c.imm);
    assert_eq!(release(def), 0);
    // CreateTexture2D1: D3D11_TEXTURE2D_DESC1 (the old description, then TextureLayout)
    let td = mem(12);
    put(td, &[64, 32, 1, 1, 28, 1, 0, 0, 0x8, 0, 0, 0]);
    assert_eq!(call1(m(IF_DEVICE, 54), &[c.dev, td, 0, c.o]), 0);
    let tex = rd1(c.o);
    assert_eq!(iface_of(tex), IF_TEX2D);
    let got = mem(11);
    call1(m(IF_TEX2D, 10), &[tex, got]);
    assert_eq!((rd1(got), rd1(got + 4), rd1(got + 16)), (64, 32, 28));
    // CreateShaderResourceView1 with no description: the whole texture
    assert_eq!(call1(m(IF_DEVICE, 57), &[c.dev, tex, 0, c.o]), 0);
    assert_eq!(release(rd1(c.o)), 0);
    assert_eq!(release(tex), 0);
    // CreateQuery1: D3D11_QUERY_DESC1 (query, misc flags, context type)
    let qd = mem(3);
    put(qd, &[0, 0, 0]);
    assert_eq!(call1(m(IF_DEVICE, 60), &[c.dev, qd, c.o]), 0);
    assert_eq!(release(rd1(c.o)), 0);
    // RegisterDeviceRemovedEvent: a cookie; no event, no cookie: E_INVALIDARG
    put(c.o, &[0xdead]);
    assert_eq!(call1(m(IF_DEVICE, 65), &[c.dev, 0x40, c.o]), 0);
    assert_eq!(rd1(c.o), 0);
    assert_eq!(call1(m(IF_DEVICE, 65), &[c.dev, 0, c.o]), E_INVALIDARG);
    // ID3D11DeviceContext3: no hardware protection; Flush1 flushes
    put(c.o, &[0xdead]);
    call1(m(IF_CONTEXT, 146), &[c.imm, c.o]);
    assert_eq!(rd1(c.o), 0);
    call1(m(IF_CONTEXT, 144), &[c.imm, 0, 0]);
    release(c.imm);
    release(c.dev);
}

/// Wine's test_private_data on one device per process: "two" devices (the same one) with the
/// one stored in the other's private data, a texture holding it too; everything goes at the end.
#[test]
fn device_in_its_own_private_data() {
    let _g = setup();
    let c = Ctx::new();
    let (dev_out, level, ctx_out) = (mem(2), mem(2), mem(2));
    assert_eq!(call1(CREATE_DEVICE, &[0, 1, 0, 0, 0, 0, 7, 0, 0, dev_out, level, 0]), 0);
    let _ = ctx_out;
    let test_object = rd1(dev_out);
    release(c.imm);
    let guid = mem(4);
    put(guid, &[0xfdb37466, 1, 2, 3]);
    let td = mem(11);
    put(td, &[512, 512, 1, 1, 28, 1, 0, 0, 0x20, 0, 0]);
    assert_eq!(call1(m(IF_DEVICE, 5), &[c.dev, td, 0, c.o]), 0);
    let tex = rd1(c.o);
    let iid = mem(4);
    put(iid, &[0xcafcb56c, 0, 0, 0]);
    assert_eq!(call1(m(IF_TEX2D, 0), &[tex, iid, c.o]), 0);
    let surface = rd1(c.o);
    let base = refs(test_object);
    // SetPrivateDataInterface(device, guid, test_object) twice, NULL, again, then data over it
    let spdi = |o: u32, v: u32| call1(m(iface_of(o), if iface_of(o) == IF_DEVICE { 36 } else { 6 }), &[o, guid, v]);
    assert_eq!(spdi(c.dev, test_object), 0);
    assert_eq!(refs(test_object), base + 1);
    assert_eq!(spdi(c.dev, test_object), 0);
    assert_eq!(refs(test_object), base + 1);
    assert_eq!(spdi(c.dev, 0), 0);
    assert_eq!(refs(test_object), base);
    assert_eq!(spdi(c.dev, test_object), 0);
    let data = mem(4);
    assert_eq!(call1(m(IF_DEVICE, 35), &[c.dev, guid, 16, data]), 0);
    assert_eq!(refs(test_object), base);
    assert_eq!(spdi(c.dev, test_object), 0);
    // GetPrivateData gives one more
    let (size, ptr_out) = (mem(1), mem(2));
    put(size, &[8]);
    assert_eq!(call1(m(IF_DEVICE, 34), &[c.dev, guid, size, ptr_out]), 0);
    assert_eq!(refs(test_object), base + 2);
    release(rd1(ptr_out));
    // the texture holds it too; its surface face reads the same store
    assert_eq!(spdi(tex, test_object), 0);
    put(size, &[4]);
    assert_eq!(call1(m(IF_SURFACE, 5), &[surface, guid, size, ptr_out]), 0);
    release(rd1(ptr_out));
    assert_eq!(refs(test_object), base + 2);
    // ... and its D3D10 face
    put(iid, &[0x9b7e4c04, 0, 0, 0]);
    if call1(m(IF_TEX2D, 0), &[tex, iid, c.o]) == 0 {
        let t10 = rd1(c.o);
        put(size, &[4]);
        assert_eq!(call1(m(d3d10::IF10_TEX2D, 4), &[t10, guid, size, ptr_out]), 0);
        release(rd1(ptr_out));
        release(t10);
    }
    assert_eq!(refs(test_object), base + 2);
    release(surface);
    // (the texture's own reference of the device and the one its private data held)
    assert_eq!(release(tex), 0);
    assert_eq!(refs(test_object), base);
    // the device's two references and the one it holds of itself: after the second, gone
    assert_eq!(release(c.dev), 2);
    assert_eq!(release(test_object), 0);
    // a device made again starts with nothing left
    let c2 = Ctx::new();
    assert_eq!(release(c2.imm), 0);
    assert_eq!(release(c2.dev), 0);
}

/// Wine's test_clear_state: a blend and a depth stencil state set, read back, cleared and let
/// go of leave nothing holding the device.
#[test]
fn state_objects_let_go() {
    let _g = setup();
    let c = Ctx::new();
    let base = refs(c.dev);
    let bd = mem(66);
    put(bd, &[0; 66]);
    put(bd + 8, &[0, 2, 1, 1, 2, 1, 1, 0xf]);
    assert_eq!(call1(m(IF_DEVICE, 20), &[c.dev, bd, c.o]), 0);
    let blend = rd1(c.o);
    let dd = mem(13);
    put(dd, &[1, 1, 2, 0, 0xff, 0xff, 1, 1, 1, 8, 1, 1, 1]);
    assert_eq!(call1(m(IF_DEVICE, 21), &[c.dev, dd, c.o]), 0);
    let ds = rd1(c.o);
    assert_eq!(refs(c.dev), base + 2);
    let factor = mem(4);
    call1(m(IF_CONTEXT, 35), &[c.imm, blend, factor, 0xff00_ff00]);
    call1(m(IF_CONTEXT, 36), &[c.imm, ds, 3]);
    let (o, f, mask) = (mem(2), mem(4), mem(1));
    call1(m(IF_CONTEXT, 91), &[c.imm, o, f, mask]);
    assert_eq!(rd1(o), blend);
    release(rd1(o));
    call1(m(IF_CONTEXT, 91), &[c.imm, o, 0, 0]);
    release(rd1(o));
    call1(m(IF_CONTEXT, 92), &[c.imm, o, mask]);
    release(rd1(o));
    call1(m(IF_CONTEXT, 92), &[c.imm, o, 0]);
    release(rd1(o));
    // no out pointer: no reference (Wine's test asks with every pointer NULL in turn)
    let before = (refs(blend), refs(ds));
    call1(m(IF_CONTEXT, 91), &[c.imm, 0, 0, 0]);
    call1(m(IF_CONTEXT, 91), &[c.imm, 0, f, mask]);
    call1(m(IF_CONTEXT, 92), &[c.imm, 0, mask]);
    call1(m(IF_CONTEXT, 81), &[c.imm, 0, 1, 0]);
    assert_eq!((refs(blend), refs(ds)), before);
    call1(m(IF_CONTEXT, 110), &[c.imm]);
    assert_eq!(release(ds), 0);
    assert_eq!(release(blend), 0);
    assert_eq!(refs(c.dev), base);
    release(c.imm);
    release(c.dev);
}

/// Wine's test_create_rasterizer_state / test_create_sampler_state / CreateBlendState1 and the
/// rules Windows has for the descriptions it gives back: the D3D11_*_DESC1/2 fields at the
/// ends, the sampler fields that do not apply cleared, one object per description.
#[test]
fn state_descriptions() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let c = Ctx::new();
    let qi = |obj: u32, g: [u32; 4]| -> Result<u32, u32> {
        let p = mem(4);
        put(p, &g);
        let r = call1(m(iface_of(obj), 0), &[obj, p, c.o]);
        if r == 0 { Ok(rd1(c.o)) } else { Err(r) }
    };
    // rasterizer: CreateRasterizerState gives ForcedSampleCount 0 and ConservativeRaster OFF
    let base = [3, 3, 0, 0, 0, 0, 1, 0, 0, 0];
    let desc = mem(12);
    put(desc, &base);
    assert_eq!(call1(m(IF_DEVICE, 22), &[c.dev, desc, c.o]), 0);
    let rs = rd1(c.o);
    let rs2 = qi(rs, [0x6fbd02fb, 0x46c4209f, 0xd12e59b0, 0xaca68655]).expect("ID3D11RasterizerState2");
    let out = mem(13);
    put(out, &[0xcccccccc; 13]);
    call1(m(IF_RASTER, 9), &[rs2, out]);
    assert_eq!((0..13).map(|i| rd1(out + 4 * i)).collect::<Vec<_>>(), [&base[..], &[0, 0, 0xcccccccc]].concat());
    put(out, &[0xcccccccc; 13]);
    call1(m(IF_RASTER, 7), &[rs2, out]);
    assert_eq!(rd1(out + 40), 0xcccccccc, "GetDesc writes D3D11_RASTERIZER_DESC only");
    put(out, &[0xcccccccc; 13]);
    call1(m(IF_RASTER, 8), &[rs2, out]);
    assert_eq!((rd1(out + 40), rd1(out + 44)), (0, 0xcccccccc));
    release(rs2);
    // the same description through CreateRasterizerState1/2 is the same object
    put(desc, &[&base[..], &[0, 0]].concat());
    assert_eq!(call1(m(IF_DEVICE, 56), &[c.dev, desc, c.o]), 0);
    assert_eq!(rd1(c.o), rs);
    release(rd1(c.o));
    assert_eq!(call1(m(IF_DEVICE, 46), &[c.dev, desc, c.o]), 0);
    assert_eq!(rd1(c.o), rs);
    release(rd1(c.o));
    put(desc, &[&base[..], &[4, 0]].concat());
    assert_eq!(call1(m(IF_DEVICE, 46), &[c.dev, desc, c.o]), 0);
    let forced = rd1(c.o);
    assert_ne!(forced, rs);
    call1(m(IF_RASTER, 9), &[forced, out]);
    assert_eq!((rd1(out + 40), rd1(out + 44)), (4, 0));
    release(forced);
    // ForcedSampleCount as Windows takes it (winref d3d11copy "raster1"): 0, 1, 4, 8, and 1
    // only without MultisampleEnable
    for forced in [0, 1, 4, 8, 16, 3, 32] {
        for ms in [0, 1] {
            let mut b = base;
            b[8] = ms;
            put(desc, &[&b[..], &[forced]].concat());
            let ok = matches!(forced, 0 | 1 | 4 | 8) && !(forced == 1 && ms == 1);
            let hr = call1(m(IF_DEVICE, 46), &[c.dev, desc, c.o]);
            assert_eq!(hr == 0, ok, "forced {forced} ms {ms}");
            if hr == 0 {
                release(rd1(c.o));
            }
        }
    }
    // CreateRasterizerState2 with ConservativeRaster ON: refused (as WARP)
    for (cons, ok) in [(0, true), (1, false), (2, false)] {
        put(desc, &[&base[..], &[0, cons]].concat());
        let hr = call1(m(IF_DEVICE, 56), &[c.dev, desc, c.o]);
        assert_eq!(hr == 0, ok, "conservative {cons}");
        if hr == 0 {
            release(rd1(c.o));
        }
    }
    assert_eq!(release(rs), 0);

    // samplers (filter, U, V, W, bias, MaxAnisotropy, ComparisonFunc, border, min, max LOD)
    let one = 1f32.to_bits();
    let sampler = |filter: u32, w: u32| -> Vec<u32> {
        let s = mem(13);
        put(s, &[filter, 1, 1, w, 0, 16, 8, 0, one, 0, one, 0, 16f32.to_bits()]);
        assert_eq!(call1(m(IF_DEVICE, 23), &[c.dev, s, c.o]), 0);
        let o = rd1(c.o);
        let out = mem(13);
        call1(m(IF_SAMPLER, 7), &[o, out]);
        release(o);
        (0..13).map(|i| rd1(out + 4 * i)).collect()
    };
    // MIN_MAG_MIP_LINEAR, all WRAP: no anisotropy, NEVER, no border colour
    assert_eq!(sampler(0x15, 1)[5..11], [0, 1, 0, 0, 0, 0]);
    // a BORDER address keeps the colour
    assert_eq!(sampler(0x15, 4)[5..11], [0, 1, 0, one, 0, one]);
    // ANISOTROPIC keeps MaxAnisotropy; COMPARISON_ANISOTROPIC both
    assert_eq!(sampler(0x55, 1)[5..7], [16, 1]);
    assert_eq!(sampler(0xd5, 1)[5..7], [16, 8]);
    // COMPARISON_MIN_MAG_MIP_POINT: the function only; MINIMUM_* / MAXIMUM_* are not comparisons
    assert_eq!(sampler(0x80, 1)[5..7], [0, 8]);
    assert_eq!(sampler(0x100, 1)[5..7], [0, 1]);
    assert_eq!(sampler(0x180, 1)[5..7], [0, 1]);

    // blend: CreateBlendState1's LogicOpEnable / LogicOp come back from GetDesc1, GetDesc
    // writes D3D11_BLEND_DESC only
    // (IndependentBlendEnable: without it every target is target 0's, logic op included; with
    // it a logic op is refused, winref d3d11copy)
    let b1 = mem(82);
    let mut v = vec![0, 1];
    for t in 0..8 {
        v.extend([0, (t == 0) as u32, 2, 1, 1, 2, 1, 1, if t == 0 { 4 } else { 0 }, 0xf]);
    }
    put(b1, &v);
    assert_eq!(call1(m(IF_DEVICE, 45), &[c.dev, b1, c.o]), E_INVALIDARG);
    v[1] = 0;
    put(b1, &v);
    assert_eq!(call1(m(IF_DEVICE, 45), &[c.dev, b1, c.o]), 0);
    let bs = rd1(c.o);
    let out = mem(83);
    put(out, &[0xcccccccc; 83]);
    call1(m(IF_BLEND, 8), &[bs, out]);
    let mut want = v.clone();
    for t in 1..8 {
        want[2 + t * 10 + 1] = 1;
        want[2 + t * 10 + 8] = 4;
    }
    assert_eq!((0..82).map(|i| rd1(out + 4 * i)).collect::<Vec<_>>(), want);
    put(out, &[0xcccccccc; 83]);
    call1(m(IF_BLEND, 7), &[bs, out]);
    assert_eq!(rd1(out + 4 * 66), 0xcccccccc);
    assert_eq!(rd1(out + 4 * 9), 0xf);
    // the same description without logic ops is another object, CreateBlendState's is the same
    let b0 = mem(66);
    let mut w = vec![0, 1];
    for _ in 0..8 {
        w.extend([0, 2, 1, 1, 2, 1, 1, 0xf]);
    }
    put(b0, &w);
    assert_eq!(call1(m(IF_DEVICE, 20), &[c.dev, b0, c.o]), 0);
    let plain = rd1(c.o);
    assert_ne!(plain, bs);
    v[1] = 1;
    v[3] = 0;
    v[10] = 0;
    put(b1, &v);
    assert_eq!(call1(m(IF_DEVICE, 45), &[c.dev, b1, c.o]), 0);
    assert_eq!(rd1(c.o), plain);
    release(plain);
    assert_eq!(release(plain), 0);
    assert_eq!(release(bs), 0);
}

/// A 2D texture of one level and one layer answers IDXGISurface, IDXGISurface1 and IDXGISurface2
/// with one face (AdvHD asks for IDXGISurface2 to make a Direct2D bitmap of it), and
/// IDXGISurface2::GetResource gives the texture back with subresource 0; one of two levels has
/// none (winref d3d11api "surface").
#[test]
fn surface2_of_a_texture() {
    let _g = setup();
    let c = Ctx::new();
    let td = mem(11);
    let iid = mem(4);
    for (mips, has) in [(1, true), (2, false)] {
        put(td, &[64, 32, mips, 1, 87, 1, 0, 0, 0x28, 0, 0]);
        assert_eq!(call1(m(IF_DEVICE, 5), &[c.dev, td, 0, c.o]), 0);
        let tex = rd1(c.o);
        let base = refs(tex);
        let mut faces = Vec::new();
        for first in [0xcafcb56c, 0x4ae63092, 0xaba496dd] {
            put(iid, &[first, 0, 0, 0]);
            let hr = call1(m(IF_TEX2D, 0), &[tex, iid, c.o]);
            assert_eq!(hr == 0, has, "{first:#x} mips {mips}");
            if hr == 0 {
                faces.push(rd1(c.o));
            } else {
                assert_eq!(rd1(c.o), 0);
            }
        }
        if has {
            assert!(faces.windows(2).all(|w| w[0] == w[1]), "{faces:x?}");
            assert_eq!(refs(tex), base + 3);
            let (out, sub) = (mem(2), mem(1));
            put(sub, &[77]);
            put(iid, &[0x6f15aaf2, 0, 0, 0]);
            assert_eq!(call1(m(IF_SURFACE, 13), &[faces[0], iid, out, sub]), 0);
            assert_eq!((rd1(out), rd1(sub)), (tex, 0));
            assert_eq!(refs(tex), base + 4);
            release(rd1(out));
            // (an interface the texture lacks: E_NOINTERFACE, nothing written)
            put(iid, &[0x48570b85, 0, 0, 0]);
            assert_ne!(call1(m(IF_SURFACE, 13), &[faces[0], iid, out, 0]), 0);
            for f in faces {
                release(f);
            }
        }
        assert_eq!(refs(tex), base);
        release(tex);
    }
}

/// Allocations of at least this many bytes fail (the guest's heap out) under `SHORT`.
static SHORT_FROM: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(u32::MAX);
unsafe extern "C" fn short_alloc(n: u32) -> *mut u8 {
    if n >= SHORT_FROM.load(std::sync::atomic::Ordering::Relaxed) {
        return std::ptr::null_mut();
    }
    alloc(n)
}
static SHORT: crate::d3d9::HostCalls = crate::d3d9::HostCalls { alloc: short_alloc, free, client_rect };
fn call_short(id: u32, args: &[u32]) -> u32 {
    unsafe { call(id, args.as_ptr(), &SHORT) }
}

/// The host's allocator giving nothing (the review: Map, a deferred Map and a deferred list's
/// replay wrote to address 0): Map fails with E_OUTOFMEMORY and clears pData, a deferred Map
/// too, a list replayed while it gives nothing drops the update, and every one works again
/// once it gives memory. Any size that fails, any order: a model of the resource's contents.
#[test]
fn allocator_gives_nothing() {
    use proptest::prelude::*;
    let _g = setup();
    let c = Ctx::new();
    let data: Vec<u32> = (0..16).collect();
    let dynamic = c.buffer(64, 2, 1, 0x10000, Some(&data));
    // 16x16 B8G8R8A8 staging
    let d = mem(11);
    put(d, &[16, 16, 1, 1, 87, 1, 0, 3, 0, 0x30000, 0]);
    assert_eq!(call1(m(IF_DEVICE, 5), &[c.dev, d, 0, c.o]), 0);
    let staging = rd1(c.o);
    let def = c.deferred();
    let mut model = data.clone();
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig { cases: 64, ..ProptestConfig::default() });
    let model_cell = std::cell::RefCell::new(&mut model);
    // (a staging texture keeps its map memory after Unmap: once mapped, maps take no more)
    let kept = std::cell::Cell::new(false);
    runner
        .run(&(prop_oneof![Just(16u32), Just(64), Just(1024), Just(u32::MAX)], 0u32..1000), |(from, k)| {
            let mut model = model_cell.borrow_mut();
            SHORT_FROM.store(from, std::sync::atomic::Ordering::Relaxed);
            // immediate Map(READ) of the staging texture (1 KiB a map)
            let out = mem(3);
            put(out, &[0xdead, 0, 0]);
            let hr = call_short(m(IF_CONTEXT, 14), &[c.imm, staging, 0, 1, 0, out]);
            if from <= 1024 && !kept.get() {
                prop_assert_eq!((hr, rd1(out)), (E_OUTOFMEMORY, 0));
            } else {
                kept.set(true);
                prop_assert_eq!(hr, 0);
                prop_assert!(rd1(out) != 0);
                call_short(m(IF_CONTEXT, 15), &[c.imm, staging, 0]);
            }
            // a deferred WRITE_DISCARD of the buffer (64 bytes)
            let out = mem(3);
            put(out, &[0xdead, 0, 0]);
            let hr = call_short(m(IF_CONTEXT, 14), &[def, dynamic, 0, 4, 0, out]);
            SHORT_FROM.store(u32::MAX, std::sync::atomic::Ordering::Relaxed);
            if from <= 64 {
                prop_assert_eq!((hr, rd1(out)), (E_OUTOFMEMORY, 0));
            } else {
                prop_assert_eq!(hr, 0);
                let v: Vec<u32> = (0..16).map(|i| k * 100 + i).collect();
                put(rd1(out), &v);
                call1(m(IF_CONTEXT, 15), &[def, dynamic, 0]);
                if k % 3 != 0 {
                    **model = v;
                }
            }
            let list = c.finish(def, 0);
            // replayed while the allocator gives nothing: the update's copy is not made
            SHORT_FROM.store(if k % 3 == 0 { 16 } else { u32::MAX }, std::sync::atomic::Ordering::Relaxed);
            unsafe { call(m(IF_CONTEXT, 58), [c.imm, list, 1].as_ptr(), &SHORT) };
            SHORT_FROM.store(u32::MAX, std::sync::atomic::Ordering::Relaxed);
            prop_assert_eq!(release(list), 0);
            prop_assert_eq!(&contents(dynamic, 16), &**model);
            Ok(())
        })
        .unwrap();
    // state objects' GetDesc with a NULL description: nothing written
    let bd = mem(66);
    put(bd, &[0; 66]);
    put(bd + 8, &[0, 2, 1, 1, 2, 1, 1, 0xf]);
    assert_eq!(call1(m(IF_DEVICE, 20), &[c.dev, bd, c.o]), 0);
    let blend = rd1(c.o);
    for meth in [7, 8] {
        call1(m(IF_BLEND, meth), &[blend, 0]);
    }
    assert_eq!(release(blend), 0);
    assert_eq!(release(def), 0);
    assert_eq!(release(staging), 0);
    assert_eq!(release(dynamic), 0);
    release(c.imm);
    release(c.dev);
}
