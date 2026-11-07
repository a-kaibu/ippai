//! Random Direct3D 9 calls through the real entry point (`vnhost_d3d9_call`), recorded as a
//! trace for TLC to check against formal/tla/ComRefs.tla (formal/trace.sh): what each call
//! returned and handed back, and which of the objects seen so far are alive after it.
//!
//! `IPPAI_TRACE_DIR=dir` writes `dir/d3d9-<seed>/ComTrace.tla`; `IPPAI_TRACE_SEEDS=n` (default 20).

use super::*;
use std::fmt::Write as _;
use std::sync::Mutex;

pub(crate) struct Rng(pub u64);
impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    pub(crate) fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

/// Guest memory: below 4G, never given back (tests are short).
static ARENA: Mutex<(u32, u32)> = Mutex::new((0, 0));

pub(crate) unsafe extern "C" fn alloc(n: u32) -> *mut u8 {
    let mut a = ARENA.lock().unwrap();
    if a.0 == 0 {
        let size = 512 << 20;
        let p = libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_32BIT,
            -1,
            0,
        );
        assert!(p != libc::MAP_FAILED);
        *a = (p as usize as u32, p as usize as u32 + size as u32);
    }
    let p = a.0;
    a.0 += (n + 15) & !15;
    assert!(a.0 <= a.1, "test arena full");
    p as *mut u8
}
unsafe extern "C" fn free(_: *mut u8) {}
/// Runs `f` on a thread whose stack and heap are below 4 GB: the Direct3D 8 layer hands the
/// Direct3D 9 one structures on its stack and heap by 32-bit address, as it does in wasm32.
pub(crate) fn on_low_stack(f: fn()) {
    unsafe {
        let size = 16 << 20;
        let st = libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_32BIT,
            -1,
            0,
        );
        assert!(st != libc::MAP_FAILED);
        let mut attr: libc::pthread_attr_t = std::mem::zeroed();
        libc::pthread_attr_init(&mut attr);
        libc::pthread_attr_setstack(&mut attr, st, size);
        extern "C" fn start(f: *mut libc::c_void) -> *mut libc::c_void {
            let f: fn() = unsafe { std::mem::transmute(f) };
            LOW_HEAP.with(|l| l.set(true));
            let r = std::panic::catch_unwind(f);
            LOW_HEAP.with(|l| l.set(false));
            match r {
                Ok(()) => std::ptr::null_mut(),
                Err(e) => Box::into_raw(Box::new(e)) as *mut libc::c_void,
            }
        }
        let mut t: libc::pthread_t = 0;
        assert_eq!(
            libc::pthread_create(&mut t, &attr, start, f as *mut libc::c_void),
            0
        );
        let mut ret = std::ptr::null_mut();
        libc::pthread_join(t, &mut ret);
        libc::munmap(st, size);
        if !ret.is_null() {
            std::panic::resume_unwind(*Box::from_raw(ret as *mut Box<dyn std::any::Any + Send>));
        }
    }
}

thread_local! {
    static LOW_HEAP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The test binary's allocator: the system's, but on `on_low_stack`'s thread a bump heap
/// below 4 GB (never freed: those tests allocate little).
struct LowHeap;
static LOW: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static LOW_AT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const LOW_SIZE: usize = 256 << 20;

impl LowHeap {
    fn low(p: *mut u8) -> bool {
        let b = LOW.load(std::sync::atomic::Ordering::Relaxed);
        b != 0 && (p as usize).wrapping_sub(b) < LOW_SIZE
    }
}

unsafe impl std::alloc::GlobalAlloc for LowHeap {
    unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
        if !LOW_HEAP.try_with(|x| x.get()).unwrap_or(false) {
            return std::alloc::System.alloc(l);
        }
        use std::sync::atomic::Ordering::*;
        if LOW.load(Acquire) == 0 {
            let p = libc::mmap(
                std::ptr::null_mut(),
                LOW_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_32BIT | libc::MAP_NORESERVE,
                -1,
                0,
            );
            assert!(p != libc::MAP_FAILED);
            if LOW
                .compare_exchange(0, p as usize, AcqRel, Acquire)
                .is_err()
            {
                libc::munmap(p, LOW_SIZE);
            }
        }
        let base = LOW.load(Acquire);
        let mut at = LOW_AT.load(Relaxed);
        loop {
            let start = (at + l.align() - 1) & !(l.align() - 1);
            let end = start + l.size().max(1);
            if end > LOW_SIZE {
                return std::ptr::null_mut();
            }
            match LOW_AT.compare_exchange_weak(at, end, Relaxed, Relaxed) {
                Ok(_) => return (base + start) as *mut u8,
                Err(a) => at = a,
            }
        }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
        if !Self::low(p) {
            std::alloc::System.dealloc(p, l)
        }
    }
    unsafe fn realloc(&self, p: *mut u8, l: std::alloc::Layout, size: usize) -> *mut u8 {
        if !Self::low(p) && !LOW_HEAP.try_with(|x| x.get()).unwrap_or(false) {
            return std::alloc::System.realloc(p, l, size);
        }
        let n = self.alloc(std::alloc::Layout::from_size_align_unchecked(
            size,
            l.align(),
        ));
        if !n.is_null() {
            std::ptr::copy_nonoverlapping(p, n, l.size().min(size));
            self.dealloc(p, l);
        }
        n
    }
}

#[global_allocator]
static HEAP: LowHeap = LowHeap;
unsafe extern "C" fn client_rect(_: u32, r: *mut i32) -> i32 {
    for (i, v) in [0, 0, 64, 64].into_iter().enumerate() {
        *r.add(i) = v;
    }
    1
}
pub(crate) static CALLS: HostCalls = HostCalls {
    alloc,
    free,
    client_rect,
};

/// One trace step, as a ComRefs.tla record.
#[derive(Default, Clone)]
pub(crate) struct Step {
    pub op: &'static str,
    pub o: u32,
    pub dev: u32,
    pub d3d: u32,
    pub slot: &'static str,
    pub v: u32,
    pub l: u32,
    pub sb: u32,
    pub lv: Vec<u32>,
    pub bb: u32,
    pub sc: u32,
    pub ads: u32,
    pub ret: String,
    pub got: u32,
    pub alive: Vec<u32>,
}

pub(crate) fn hr(r: u32) -> String {
    match r {
        0 => "\"ok\"".into(),
        D3DERR_INVALIDCALL => "\"invalid\"".into(),
        D3DERR_NOTFOUND => "\"notfound\"".into(),
        0x887a0001 => "\"invalid\"".into(),
        0x80070057 => "\"invalid\"".into(),
        _ => format!("\"hr{r:x}\""),
    }
}

/// Writes `dir/name/<module>.tla` (the trace) and `dir/name/SPEC` (the spec it is checked
/// against: formal/tla/<spec>.tla with <spec>Trace.tla and .cfg) when IPPAI_TRACE_DIR is set.
pub(crate) fn write_module(name: &str, spec: &str, module: &str, text: &str) {
    let Some(dir) = std::env::var_os("IPPAI_TRACE_DIR") else {
        return;
    };
    let dir = std::path::Path::new(&dir).join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{module}.tla")), text).unwrap();
    std::fs::write(dir.join("SPEC"), spec).unwrap();
}

pub(crate) fn write_trace(name: &str, steps: &[Step]) {
    let set = |v: &[u32]| {
        v.iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut s = String::from("---- MODULE ComTrace ----\nEXTENDS Sequences\nTrace == <<\n");
    for (i, t) in steps.iter().enumerate() {
        let _ = write!(
            s,
            "  [op |-> \"{}\", o |-> {}, dev |-> {}, d3d |-> {}, slot |-> \"{}\", v |-> {}, l |-> {}, sb |-> {}, lv |-> <<{}>>, bb |-> {}, sc |-> {}, ads |-> {}, ret |-> {}, got |-> {}, alive |-> {{{}}}]{}\n",
            t.op, t.o, t.dev, t.d3d, t.slot, t.v, t.l, t.sb, set(&t.lv), t.bb, t.sc, t.ads, t.ret, t.got, set(&t.alive),
            if i + 1 < steps.len() { "," } else { "" }
        );
    }
    s.push_str(">>\n====\n");
    write_module(name, "ComRefs", "ComTrace", &s);
}

pub(crate) fn seeds() -> u64 {
    std::env::var("IPPAI_TRACE_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20)
}

/// The page's executor (render/d3d.rs) on a fresh model of WebGPU (render/model.rs), run in this
/// thread whenever the guest waits for it (and at gpu::pump).
pub(crate) fn attach_page() {
    crate::render::model::reset();
    let page = std::rc::Rc::new(std::cell::RefCell::new(crate::render::d3d::D3d::new()));
    let pg = page.clone();
    gpu::set_pump(Some(Box::new(move || {
        while gpu::run_next(|c, data, blob| {
            let mut d = pg.borrow_mut();
            d.run(c, data, blob);
            for t in crate::render::model::with(|m| m.finish_maps()) {
                d.mapped(t, true);
            }
        }) {}
    })));
}

/// (one test at a time drives the global tables)
pub(crate) static SERIAL: Mutex<()> = Mutex::new(());

fn call(iface: u32, m: u32, args: &[u32]) -> u32 {
    // (the guest's arguments are on its stack, with the caller's frame after them: the
    // Direct3D 8 methods that forward to Direct3D 9 read 10 words whatever the method takes)
    let mut a = args.to_vec();
    a.resize(a.len().max(16), 0);
    unsafe { vnhost_d3d9_call(iface << 8 | m, a.as_ptr(), &CALLS) }
}

fn out() -> u32 {
    unsafe { alloc(16) as u32 }
}

fn rd(p: u32) -> u32 {
    unsafe { *(p as *const u32) }
}

fn with<R>(f: impl FnOnce(&mut D3d) -> R) -> R {
    f(D3D.lock().unwrap().as_mut().unwrap())
}

fn id(addr: u32) -> u32 {
    with(|d| d.id_of(addr))
}

fn pp(autodepth: bool) -> u32 {
    let p = out_n(14);
    let v = [64, 64, 22, 1, 0, 0, 1, 1, 1, autodepth as u32, 75, 0, 0, 0];
    for (i, x) in v.into_iter().enumerate() {
        unsafe { *((p + 4 * i as u32) as *mut u32) = x };
    }
    p
}

fn out_n(n: u32) -> u32 {
    unsafe { alloc(4 * n) as u32 }
}

/// The application: what it holds (one entry a reference, by the address it got).
struct App {
    held: Vec<u32>,
    known: Vec<u32>,
    steps: Vec<Step>,
}

impl App {
    fn push(&mut self, mut t: Step) {
        for x in [t.o, t.got, t.bb, t.sc, t.ads]
            .into_iter()
            .chain(t.lv.iter().copied())
        {
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
    /// A held reference whose object `f` accepts.
    fn pick(&self, r: &mut Rng, f: impl Fn(u32, u32) -> bool) -> Option<u32> {
        let c: Vec<u32> = self
            .held
            .iter()
            .copied()
            .filter(|&a| {
                with(|d| {
                    d.objs
                        .get(&d.id_of(a))
                        .is_some_and(|o| f(o.iface, o.data_kind()))
                })
            })
            .collect();
        (!c.is_empty()).then(|| c[r.below(c.len())])
    }
    fn release(&mut self, addr: u32) {
        let i = self.held.iter().position(|&a| a == addr).unwrap();
        self.held.swap_remove(i);
        let o = id(addr);
        let iface = with(|d| d.objs[&o].iface);
        let r = call(iface, 2, &[addr]);
        self.push(Step {
            op: "Release",
            o,
            ret: r.to_string(),
            ..Default::default()
        });
    }
}

impl Obj {
    /// (1: a render target surface or level)
    fn data_kind(&self) -> u32 {
        match &self.data {
            Data::Surface(s) => (s.usage & 1 != 0) as u32,
            _ => 0,
        }
    }
}

fn buffers(t: &mut Step) {
    with(|d| {
        let dev = d.dev.as_ref().unwrap();
        (t.bb, t.sc, t.ads) = (dev.backbuffer, dev.swapchain, dev.auto_depth);
    });
}

fn run(seed: u64) {
    let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut app = App {
        held: Vec::new(),
        known: Vec::new(),
        steps: Vec::new(),
    };
    let o = out();
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d_addr = rd(o);
    let d3d = id(d3d_addr);
    app.held.push(d3d_addr);
    app.push(Step {
        op: "CreateD3D",
        o: d3d,
        ret: hr(0),
        got: d3d,
        ..Default::default()
    });
    let o = out();
    let res = call(IF_D3D9, 16, &[d3d_addr, 0, 1, 1, 0x40, pp(r.chance(50)), o]);
    assert_eq!(res, 0);
    let dev_addr = rd(o);
    let dev = id(dev_addr);
    app.held.push(dev_addr);
    let mut t = Step {
        op: "CreateDevice",
        o: dev,
        d3d,
        ret: hr(0),
        got: dev,
        ..Default::default()
    };
    buffers(&mut t);
    app.push(t);
    let tex_slots = ["tex0", "tex1"];
    for _ in 0..120 {
        // (the device's block goes to another object once it is gone)
        if id(dev_addr) != dev || !app.held.contains(&dev_addr) {
            break;
        }
        let k = r.below(15);
        match k {
            0 => {
                let rt = r.chance(50);
                let o = out();
                let res = call(
                    IF_DEVICE,
                    23,
                    &[
                        dev_addr,
                        4,
                        4,
                        r.below(4) as u32,
                        rt as u32,
                        22,
                        if rt { 0 } else { r.below(2) as u32 },
                        o,
                        0,
                    ],
                );
                let a = rd(o);
                app.held.push(a);
                let tid = id(a);
                let lv = with(|d| match &d.objs[&tid].data {
                    Data::Texture(t) => t.levels.clone(),
                    _ => unreachable!(),
                });
                app.push(Step {
                    op: "CreateTexture",
                    o: tid,
                    lv,
                    ret: hr(res),
                    got: tid,
                    ..Default::default()
                });
            }
            1 | 2 => {
                let (m, kind) = if k == 1 { (28, "surface") } else { (26, "vb") };
                let o = out();
                let args = if k == 1 {
                    vec![dev_addr, 4, 4, 22, 0, 0, 0, o, 0]
                } else {
                    vec![dev_addr, 64, 0, 0, 0, o, 0]
                };
                let res = call(IF_DEVICE, m, &args);
                let a = rd(o);
                app.held.push(a);
                app.push(Step {
                    op: "Create",
                    o: id(a),
                    slot: kind,
                    ret: hr(res),
                    got: id(a),
                    ..Default::default()
                });
            }
            3 => {
                let Some(t) = app.pick(&mut r, |i, _| i == IF_TEXTURE) else {
                    continue;
                };
                let n = with(|d| match &d.objs[&d.id_of(t)].data {
                    Data::Texture(x) => x.levels.len() as u32,
                    _ => 0,
                });
                let l = r.below(n as usize + 1) as u32;
                let o = out();
                let res = call(IF_TEXTURE, 18, &[t, l, o]);
                let got = if res == 0 {
                    app.held.push(rd(o));
                    id(rd(o))
                } else {
                    0
                };
                app.push(Step {
                    op: "GetLevel",
                    o: id(t),
                    l,
                    ret: hr(res),
                    got,
                    ..Default::default()
                });
            }
            4 => {
                let stage = r.below(2);
                let t = if r.chance(25) {
                    None
                } else {
                    app.pick(&mut r, |i, _| i == IF_TEXTURE)
                };
                let res = call(IF_DEVICE, 65, &[dev_addr, stage as u32, t.unwrap_or(0)]);
                app.push(Step {
                    op: "Set",
                    dev,
                    slot: tex_slots[stage],
                    v: t.map_or(0, id),
                    ret: hr(res),
                    ..Default::default()
                });
            }
            5 => {
                let stage = r.below(2);
                let o = out();
                let res = call(IF_DEVICE, 64, &[dev_addr, stage as u32, o]);
                let got = id(rd(o));
                if got != 0 {
                    app.held.push(rd(o));
                }
                app.push(Step {
                    op: "Get",
                    dev,
                    slot: tex_slots[stage],
                    ret: hr(res),
                    got,
                    ..Default::default()
                });
            }
            6 => {
                let b = if r.chance(25) {
                    None
                } else {
                    app.pick(&mut r, |i, _| i == IF_VB)
                };
                let res = call(IF_DEVICE, 100, &[dev_addr, 0, b.unwrap_or(0), 0, 16]);
                app.push(Step {
                    op: "Set",
                    dev,
                    slot: "vb0",
                    v: b.map_or(0, id),
                    ret: hr(res),
                    ..Default::default()
                });
            }
            7 => {
                let (o, off, stride) = (out(), out(), out());
                let res = call(IF_DEVICE, 101, &[dev_addr, 0, o, off, stride]);
                let got = id(rd(o));
                if got != 0 {
                    app.held.push(rd(o));
                }
                app.push(Step {
                    op: "Get",
                    dev,
                    slot: "vb0",
                    ret: hr(res),
                    got,
                    ..Default::default()
                });
            }
            8 => {
                let i = r.below(2);
                let s = if r.chance(20) {
                    None
                } else {
                    app.pick(&mut r, |i, k| i == IF_SURFACE && k == 1)
                };
                let res = call(IF_DEVICE, 37, &[dev_addr, i as u32, s.unwrap_or(0)]);
                app.push(Step {
                    op: "Set",
                    dev,
                    slot: ["rt0", "rt1"][i],
                    v: s.map_or(0, id),
                    ret: hr(res),
                    ..Default::default()
                });
            }
            9 => {
                let i = r.below(2);
                let o = out();
                let (res, slot) = if r.chance(30) {
                    (call(IF_DEVICE, 18, &[dev_addr, 0, 0, 0, o]), "bb")
                } else {
                    (
                        call(IF_DEVICE, 38, &[dev_addr, i as u32, o]),
                        ["rt0", "rt1"][i],
                    )
                };
                let got = if res == 0 { id(rd(o)) } else { 0 };
                if got != 0 {
                    app.held.push(rd(o));
                }
                app.push(Step {
                    op: "Get",
                    dev,
                    slot,
                    ret: hr(res),
                    got,
                    ..Default::default()
                });
            }
            10 => {
                let o = out();
                let res = call(IF_DEVICE, 59, &[dev_addr, 1, o]);
                app.held.push(rd(o));
                app.push(Step {
                    op: "CreateStateBlock",
                    o: id(rd(o)),
                    dev,
                    ret: hr(res),
                    got: id(rd(o)),
                    ..Default::default()
                });
            }
            11 => {
                let Some(sb) = app.pick(&mut r, |i, _| i == IF_STATEBLOCK) else {
                    continue;
                };
                let cap = r.chance(50);
                let res = call(IF_STATEBLOCK, if cap { 4 } else { 5 }, &[sb]);
                app.push(Step {
                    op: if cap { "Capture" } else { "Apply" },
                    sb: id(sb),
                    dev,
                    ret: hr(res),
                    ..Default::default()
                });
            }
            12 => {
                let a = app.held[r.below(app.held.len())];
                let o = id(a);
                let iface = with(|d| d.objs[&o].iface);
                if r.chance(50) {
                    let res = call(iface, 1, &[a]);
                    app.held.push(a);
                    app.push(Step {
                        op: "AddRef",
                        o,
                        ret: res.to_string(),
                        ..Default::default()
                    });
                } else {
                    let iid = out_n(4);
                    let po = out();
                    let res = call(iface, 0, &[a, iid, po]);
                    app.held.push(rd(po));
                    app.push(Step {
                        op: "QueryInterface",
                        o,
                        ret: hr(res),
                        got: id(rd(po)),
                        ..Default::default()
                    });
                }
            }
            13 => {
                let a = app.held[r.below(app.held.len())];
                if a == dev_addr && r.chance(80) {
                    continue;
                }
                app.release(a);
            }
            _ => {
                if !r.chance(30) {
                    continue;
                }
                let res = call(IF_DEVICE, 16, &[dev_addr, pp(r.chance(50))]);
                let mut t = Step {
                    op: "Reset",
                    dev,
                    ret: hr(res),
                    ..Default::default()
                };
                buffers(&mut t);
                app.push(t);
            }
        }
    }
    // (everything back, in any order: nothing seen may be left alive)
    while !app.held.is_empty() {
        let a = app.held[r.below(app.held.len())];
        app.release(a);
    }
    let left = app.steps.last().unwrap().alive.clone();
    write_trace(&format!("d3d9-{seed}"), &app.steps);
    assert!(
        left.is_empty(),
        "seed {seed}: alive after every reference was released: {left:?}"
    );
}

#[test]
fn d3d9_object_lifetimes_trace() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    for seed in 0..seeds() {
        run(seed);
    }
}
