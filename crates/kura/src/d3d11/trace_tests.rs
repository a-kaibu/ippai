//! Random Direct3D 11 calls through the real entry point (`d3d11::call`), recorded as a trace for
//! TLC to check against formal/tla/ComRefs.tla, like d3d9/trace_tests.rs: views, swap chain
//! buffers across ResizeBuffers, the context's bindings and ClearState.

use super::*;
use crate::d3d9::trace_tests::{alloc, hr, seeds, write_trace, Rng, Step, CALLS, SERIAL};

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

fn with<R>(f: impl FnOnce(&mut D11) -> R) -> R {
    f(D11S.lock().unwrap().as_mut().unwrap())
}

fn id(addr: u32) -> u32 {
    with(|d| d.id_of(addr))
}

struct App {
    held: Vec<u32>,
    known: Vec<u32>,
    steps: Vec<Step>,
}

impl App {
    fn push(&mut self, mut t: Step) {
        for x in [t.o, t.got, t.v, t.dev, t.sb] {
            if x != 0 && !self.known.contains(&x) {
                self.known.push(x);
            }
        }
        t.alive = with(|d| {
            self.known
                .iter()
                .copied()
                .filter(|x| d.objs.contains_key(x))
                .collect()
        });
        t.alive.sort();
        self.steps.push(t);
    }
    fn pick(&self, r: &mut Rng, f: impl Fn(u32) -> bool) -> Option<u32> {
        let c: Vec<u32> = self
            .held
            .iter()
            .copied()
            .filter(|&a| with(|d| d.objs.get(&d.id_of(a)).is_some_and(|o| f(o.iface))))
            .collect();
        (!c.is_empty()).then(|| c[r.below(c.len())])
    }
    fn iface(&self, a: u32) -> u32 {
        with(|d| {
            let o = d.id_of(a);
            d.objs[&o].faces.iter().find(|f| f.1 == a).unwrap().0
        })
    }
    fn release(&mut self, a: u32) {
        let i = self.held.iter().position(|&x| x == a).unwrap();
        self.held.swap_remove(i);
        let o = id(a);
        let r = call1(m(self.iface(a), 2), &[a]);
        self.push(Step {
            op: "Release",
            o,
            ret: r.to_string(),
            ..Default::default()
        });
    }
}

fn run(seed: u64) {
    let mut r = Rng(seed.wrapping_mul(0x2545_f491_4f6c_dd1d) | 1);
    let mut app = App {
        held: Vec::new(),
        known: Vec::new(),
        steps: Vec::new(),
    };
    let desc = mem(16);
    put(desc, &[64, 64, 60, 1, 28, 0, 0, 1, 0, 0x20, 1, 1, 1, 0, 0]);
    let (sc_out, dev_out, level, ctx_out) = (mem(2), mem(2), mem(2), mem(2));
    let res = call1(
        CREATE_DEVICE,
        &[0, 1, 0, 0, 0, 0, 7, desc, sc_out, dev_out, level, ctx_out],
    );
    assert_eq!(res, 0);
    let (sc, dev, ctx) = (rd1(sc_out), rd1(dev_out), rd1(ctx_out));
    app.held.extend([sc, dev, ctx]);
    let buf = with(|d| match &d.objs[&d.id_of(sc)].data {
        Data::SwapChain(s) => s.buffer,
        _ => unreachable!(),
    });
    let ctx_id = id(ctx);
    let sc_id = id(sc);
    // (the swap chain's block goes to another object once it is gone)
    let sc_ok = |app: &App| id(sc) == sc_id && app.held.contains(&sc);
    app.push(Step {
        op: "CreateDevice11",
        o: id(sc),
        dev: id(dev),
        sb: ctx_id,
        v: buf,
        ret: hr(0),
        got: id(sc),
        ..Default::default()
    });
    let srv_slots = ["srv0", "srv1"];
    for _ in 0..120 {
        match r.below(14) {
            0 => {
                // (a texture for reading or for drawing to, not both: one bound for output
                // leaves the input slots reading it (hazards.rs), which Binds11.tla checks)
                let d = mem(11);
                let bind = if r.chance(50) { 0x8 } else { 0x20 };
                put(d, &[4, 4, r.below(3) as u32, 1, 28, 1, 0, 0, bind, 0, 0]);
                let o = mem(2);
                let res = call1(m(IF_DEVICE, 5), &[dev, d, 0, o]);
                app.held.push(rd1(o));
                app.push(Step {
                    op: "Create",
                    o: id(rd1(o)),
                    slot: "texture",
                    ret: hr(res),
                    got: id(rd1(o)),
                    ..Default::default()
                });
            }
            1 => {
                let d = mem(6);
                put(d, &[64, 0, 1, 0, 0, 0]);
                let o = mem(2);
                let res = call1(m(IF_DEVICE, 3), &[dev, d, 0, o]);
                app.held.push(rd1(o));
                app.push(Step {
                    op: "Create",
                    o: id(rd1(o)),
                    slot: "buffer",
                    ret: hr(res),
                    got: id(rd1(o)),
                    ..Default::default()
                });
            }
            2 => {
                // a shader resource or render target view of a texture
                let Some(t) = app.pick(&mut r, |i| i == IF_TEX2D) else {
                    continue;
                };
                let srv = with(
                    |d| matches!(&d.objs[&d.id_of(t)].data, Data::Texture(x) if x.bind & 0x8 != 0),
                );
                let o = mem(2);
                let res = call1(m(IF_DEVICE, if srv { 7 } else { 9 }), &[dev, t, 0, o]);
                app.held.push(rd1(o));
                app.push(Step {
                    op: "Create",
                    o: id(rd1(o)),
                    slot: "view",
                    v: id(t),
                    ret: hr(res),
                    got: id(rd1(o)),
                    ..Default::default()
                });
            }
            3 => {
                let s = r.below(2);
                let v = if r.chance(25) {
                    None
                } else {
                    app.pick(&mut r, |i| i == IF_SRV)
                };
                let p = mem(2);
                put(p, &[v.unwrap_or(0)]);
                let res = call1(m(IF_CONTEXT, 8), &[ctx, s as u32, 1, p]);
                app.push(Step {
                    op: "Set",
                    dev: ctx_id,
                    slot: srv_slots[s],
                    v: v.map_or(0, id),
                    ret: hr(res),
                    ..Default::default()
                });
            }
            4 => {
                let s = r.below(2);
                let p = mem(2);
                let res = call1(m(IF_CONTEXT, 73), &[ctx, s as u32, 1, p]);
                let got = id(rd1(p));
                if got != 0 {
                    app.held.push(rd1(p));
                }
                app.push(Step {
                    op: "Get",
                    dev: ctx_id,
                    slot: srv_slots[s],
                    ret: hr(res),
                    got,
                    ..Default::default()
                });
            }
            5 => {
                let v = if r.chance(25) {
                    None
                } else {
                    app.pick(&mut r, |i| i == IF_RTV)
                };
                let p = mem(2);
                put(p, &[v.unwrap_or(0)]);
                let res = call1(m(IF_CONTEXT, 33), &[ctx, v.is_some() as u32, p, 0]);
                app.push(Step {
                    op: "Set",
                    dev: ctx_id,
                    slot: "rtv0",
                    v: v.map_or(0, id),
                    ret: hr(res),
                    ..Default::default()
                });
            }
            6 => {
                let p = mem(2);
                let res = call1(m(IF_CONTEXT, 89), &[ctx, 1, p, 0]);
                let got = id(rd1(p));
                if got != 0 {
                    app.held.push(rd1(p));
                }
                app.push(Step {
                    op: "Get",
                    dev: ctx_id,
                    slot: "rtv0",
                    ret: hr(res),
                    got,
                    ..Default::default()
                });
            }
            7 => {
                let b = if r.chance(25) {
                    None
                } else {
                    app.pick(&mut r, |i| i == IF_BUFFER)
                };
                let (p, st, off) = (mem(2), mem(2), mem(2));
                put(p, &[b.unwrap_or(0)]);
                put(st, &[16]);
                let res = call1(m(IF_CONTEXT, 18), &[ctx, 0, 1, p, st, off]);
                app.push(Step {
                    op: "Set",
                    dev: ctx_id,
                    slot: "vb",
                    v: b.map_or(0, id),
                    ret: hr(res),
                    ..Default::default()
                });
            }
            8 => {
                let (p, st, off) = (mem(2), mem(2), mem(2));
                let res = call1(m(IF_CONTEXT, 79), &[ctx, 0, 1, p, st, off]);
                let got = id(rd1(p));
                if got != 0 {
                    app.held.push(rd1(p));
                }
                app.push(Step {
                    op: "Get",
                    dev: ctx_id,
                    slot: "vb",
                    ret: hr(res),
                    got,
                    ..Default::default()
                });
            }
            9 => {
                if !r.chance(30) {
                    continue;
                }
                let res = call1(m(IF_CONTEXT, 110), &[ctx]);
                app.push(Step {
                    op: "ClearState",
                    dev: ctx_id,
                    ret: hr(res),
                    ..Default::default()
                });
            }
            10 => {
                // a swap chain's buffer, a view's resource
                let (a, res, o) = if r.chance(50) && sc_ok(&app) {
                    let (iid, o) = (mem(4), mem(2));
                    (sc, call1(m(IF_SWAPCHAIN, 9), &[sc, 0, iid, o]), o)
                } else {
                    let Some(v) = app.pick(&mut r, |i| i == IF_SRV || i == IF_RTV) else {
                        continue;
                    };
                    let o = mem(2);
                    (v, call1(m(app.iface(v), 7), &[v, o]), o)
                };
                app.held.push(rd1(o));
                app.push(Step {
                    op: "GetHeld",
                    o: id(a),
                    ret: hr(res),
                    got: id(rd1(o)),
                    ..Default::default()
                });
            }
            11 => {
                if !r.chance(30) || !sc_ok(&app) {
                    continue;
                }
                let res = call1(m(IF_SWAPCHAIN, 13), &[sc, 0, 0, 0, 0, 0]);
                let v = with(|d| match &d.objs[&d.id_of(sc)].data {
                    Data::SwapChain(s) => s.buffer,
                    _ => unreachable!(),
                });
                app.push(Step {
                    op: "ResizeBuffers",
                    o: id(sc),
                    v,
                    ret: hr(res),
                    ..Default::default()
                });
            }
            12 => {
                let a = app.held[r.below(app.held.len())];
                let o = id(a);
                if r.chance(50) {
                    let res = call1(m(app.iface(a), 1), &[a]);
                    app.held.push(a);
                    app.push(Step {
                        op: "AddRef",
                        o,
                        ret: res.to_string(),
                        ..Default::default()
                    });
                } else {
                    let (iid, po) = (mem(4), mem(2));
                    let res = call1(m(app.iface(a), 0), &[a, iid, po]);
                    app.held.push(rd1(po));
                    app.push(Step {
                        op: "QueryInterface",
                        o,
                        ret: hr(res),
                        got: id(rd1(po)),
                        ..Default::default()
                    });
                }
            }
            _ => {
                let a = app.held[r.below(app.held.len())];
                if a == sc && r.chance(80) {
                    continue;
                }
                app.release(a);
            }
        }
    }
    let res = call1(m(IF_CONTEXT, 110), &[ctx]);
    app.push(Step {
        op: "ClearState",
        dev: ctx_id,
        ret: hr(res),
        ..Default::default()
    });
    while !app.held.is_empty() {
        let a = app.held[r.below(app.held.len())];
        app.release(a);
    }
    let left = app.steps.last().unwrap().alive.clone();
    write_trace(&format!("d3d11-{seed}"), &app.steps);
    let permanent = with(|d| [d.device, d.context]);
    assert!(
        left.iter().all(|x| permanent.contains(x)),
        "seed {seed}: alive after every reference was released: {left:?}"
    );
}

#[test]
fn d3d11_object_lifetimes_trace() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let vt = mem(256);
    put(vt, &(1..=256).map(|i| 0x1000 * i).collect::<Vec<_>>());
    call1(INIT, &[vt, 256]);
    for seed in 0..seeds() {
        run(seed);
    }
}
