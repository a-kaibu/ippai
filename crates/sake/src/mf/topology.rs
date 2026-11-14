//! Topologies for the Media Session (session.rs): MFCreateTopology, MFCreateTopologyNode, the
//! renderers' activates (MFCreateVideoRendererActivate, MFCreateAudioRendererActivate) and
//! MFCreateCollection. Each is an attribute store (mf.rs's ATTRS) with more methods; what those
//! keep is in a map by block here. A node holds its object, its downstream nodes and its
//! preferred types; a topology its nodes.

use super::*;

const E_UNEXPECTED: u32 = 0x8000_ffff;
const MF_E_NOT_FOUND: u32 = 0xc00d_36d5;
const MF_E_INVALIDINDEX: u32 = 0xc00d_36bf;

/// MF_TOPOLOGY_TYPE (TRANSFORM is 2)
pub const OUTPUT: u32 = 0;
pub const SOURCESTREAM: u32 = 1;
pub const TEE: u32 = 3;

/// A connection: the other node and its input (or output) index.
type Link = Option<(u32, u32)>;

#[derive(Default)]
pub struct Node {
    pub kind: u32,
    pub id: u64,
    pub object: u32,
    pub inputs: Vec<Link>,
    /// (each holds a reference to the node downstream)
    pub outputs: Vec<Link>,
    in_types: HashMap<u32, u32>,
    out_types: HashMap<u32, u32>,
}

pub struct Topo {
    pub id: u64,
    /// (each held)
    pub nodes: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Act {
    /// the EVR's, for this window
    Video(u32),
    /// the SAR's
    Audio,
}

static NODES: Mutex<Option<HashMap<u32, Node>>> = Mutex::new(None);
static TOPOS: Mutex<Option<HashMap<u32, Topo>>> = Mutex::new(None);
static ACTS: Mutex<Option<HashMap<u32, Act>>> = Mutex::new(None);
static COLLS: Mutex<Option<HashMap<u32, (u32, Vec<u32>)>>> = Mutex::new(None);

pub fn node<R>(b: u32, f: impl FnOnce(&mut Node) -> R) -> Option<R> {
    NODES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}
pub fn topo<R>(b: u32, f: impl FnOnce(&mut Topo) -> R) -> Option<R> {
    TOPOS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}
pub fn act(b: u32) -> Option<Act> {
    ACTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get(&b).copied()
}
fn coll<R>(b: u32, f: impl FnOnce(&mut (u32, Vec<u32>)) -> R) -> Option<R> {
    COLLS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}

/// TOPOIDs: the process in the high word (as Windows'), a count below
fn next_id() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    (std::process::id() as u64) << 32 | N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The out pointer after a 64-bit argument at x86 slots `slot - 2`, `slot - 1`.
fn after64(c: &Ctx, slot: u32) -> u32 {
    if mem::x64() { c.a(slot - 1) } else { c.a(slot) }
}

/// A new attribute store with vtable `vt` (one reference).
fn new_store(vt: u32) -> u32 {
    let b = object(vt, 1);
    ATTRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, (1, Store::default()));
    b
}
/// Release of a store whose last reference lets `gone` drop what it holds first.
fn store_release(c: &mut Ctx, gone: fn(&mut Ctx, u32)) -> u32 {
    let b = c.a(0);
    if attrs(b, |a| a.0) == Some(1) {
        gone(c, b);
    }
    at_release(c)
}

// --- nodes ----------------------------------------------------------------------------------

fn tn_qi(c: &mut Ctx) -> u32 {
    // IMFTopologyNode {83cf873a-f6da-4bc8-823f-bacfd55dc430} (IMFTopology's but the last byte)
    let g = guid_at(c.a(1));
    if is(&g, 0x83cf_873a, 0xf6da, 0x4bc8) && g[15] != 0x30 {
        putp(c.a(2), 0);
        return E_NOINTERFACE;
    }
    reader::typed_qi(c, 0x83cf_873a, 0xf6da, 0x4bc8)
}
fn node_gone(c: &mut Ctx, b: u32) {
    let Some(n) = NODES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&b) else { return };
    release(c, n.object);
    for (d, _) in n.outputs.iter().flatten() {
        node(*d, |dn| dn.inputs.iter_mut().for_each(|l| if l.is_some_and(|l| l.0 == b) { *l = None }));
        release(c, *d);
    }
    for t in n.in_types.values().chain(n.out_types.values()) {
        release(c, *t);
    }
}
fn tn_release(c: &mut Ctx) -> u32 {
    store_release(c, node_gone)
}
fn tn_set_object(c: &mut Ctx) -> u32 {
    let o = c.a(1);
    add_ref(c, o);
    let old = node(c.a(0), |n| std::mem::replace(&mut n.object, o)).unwrap_or(0);
    release(c, old);
    0
}
fn tn_get_object(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    let o = node(c.a(0), |n| n.object).unwrap_or(0);
    putp(out, o);
    if o == 0 {
        return E_FAIL;
    }
    add_ref(c, o);
    0
}
fn tn_get_type(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), node(c.a(0), |n| n.kind).unwrap_or(0));
    0
}
fn tn_get_id(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w64(c.a(1), node(c.a(0), |n| n.id).unwrap_or(0));
    0
}
fn tn_set_id(c: &mut Ctx) -> u32 {
    let id = u64_arg(c, 1, 1);
    node(c.a(0), |n| n.id = id);
    0
}
fn tn_count(c: &mut Ctx, outputs: bool) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), node(c.a(0), |n| if outputs { n.outputs.len() } else { n.inputs.len() } as u32).unwrap_or(0));
    0
}
/// Unlinks output `i` of `b` (its downstream node is handed back, still held).
fn unlink(b: u32, i: usize) -> Option<u32> {
    let (d, di) = node(b, |n| n.outputs.get_mut(i).and_then(|l| l.take())).flatten()?;
    node(d, |dn| {
        if let Some(l) = dn.inputs.get_mut(di as usize) {
            *l = None;
        }
    });
    Some(d)
}
/// ConnectOutput(DWORD output, IMFTopologyNode *down, DWORD input)
fn tn_connect(c: &mut Ctx) -> u32 {
    let (b, i, d, di) = (c.a(0), c.a(1) as usize, c.a(2), c.a(3) as usize);
    if d == 0 {
        return E_POINTER;
    }
    let (Some(k), Some(dk)) = (node(b, |n| n.kind), node(d, |n| n.kind)) else { return E_INVALIDARG };
    // (an output node has no outputs; a source node no inputs)
    if k == OUTPUT || dk == SOURCESTREAM {
        return E_FAIL;
    }
    if let Some(old) = unlink(b, i) {
        release(c, old);
    }
    // (what was connected to that input of `d` before is let go)
    if let Some((u, ui)) = node(d, |n| n.inputs.get(di).copied().flatten()).flatten() {
        if let Some(old) = unlink(u, ui as usize) {
            release(c, old);
        }
    }
    add_ref(c, d);
    node(b, |n| {
        if n.outputs.len() <= i {
            n.outputs.resize(i + 1, None);
        }
        n.outputs[i] = Some((d, di as u32));
    });
    node(d, |n| {
        if n.inputs.len() <= di {
            n.inputs.resize(di + 1, None);
        }
        n.inputs[di] = Some((b, i as u32));
    });
    0
}
fn tn_disconnect(c: &mut Ctx) -> u32 {
    match unlink(c.a(0), c.a(1) as usize) {
        Some(d) => {
            release(c, d);
            0
        }
        None => MF_E_NOT_FOUND,
    }
}
/// GetInput / GetOutput(DWORD, IMFTopologyNode **, DWORD *other)
fn tn_link(c: &mut Ctx, outputs: bool) -> u32 {
    let (i, out, oi) = (c.a(1) as usize, c.a(2), c.a(3));
    if out == 0 || oi == 0 {
        return E_POINTER;
    }
    let l = node(c.a(0), |n| (if outputs { &n.outputs } else { &n.inputs }).get(i).copied());
    match l.flatten() {
        None => E_INVALIDARG,
        Some(None) => MF_E_NOT_FOUND,
        Some(Some((o, k))) => {
            add_ref(c, o);
            putp(out, o);
            w32(oi, k);
            0
        }
    }
}
/// Set{Output,Input}PrefType(DWORD, IMFMediaType *)
fn tn_set_pref(c: &mut Ctx, outputs: bool) -> u32 {
    let (i, t) = (c.a(1), c.a(2));
    add_ref(c, t);
    let old = node(c.a(0), |n| {
        let m = if outputs { &mut n.out_types } else { &mut n.in_types };
        if t == 0 { m.remove(&i) } else { m.insert(i, t) }
    })
    .flatten();
    release(c, old.unwrap_or(0));
    0
}
fn tn_get_pref(c: &mut Ctx, outputs: bool) -> u32 {
    let (i, out) = (c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let t = node(c.a(0), |n| (if outputs { &n.out_types } else { &n.in_types }).get(&i).copied()).flatten();
    putp(out, t.unwrap_or(0));
    match t {
        Some(t) => {
            add_ref(c, t);
            0
        }
        None => MF_E_NOT_FOUND,
    }
}
/// CloneFrom(IMFTopologyNode *): its object, attributes, preferred types and id (not its
/// connections); a node of another type is refused
fn tn_clone_from(c: &mut Ctx) -> u32 {
    let (b, from) = (c.a(0), c.a(1));
    if from == 0 {
        return E_POINTER;
    }
    let Some((kind, id, o, it, ot)) = node(from, |n| (n.kind, n.id, n.object, n.in_types.clone(), n.out_types.clone())) else { return E_INVALIDARG };
    if node(b, |n| n.kind) != Some(kind) {
        return MF_E_INVALIDREQUEST;
    }
    for &t in it.values().chain(ot.values()) {
        add_ref(c, t);
    }
    add_ref(c, o);
    let old = node(b, |n| {
        n.id = id;
        (std::mem::replace(&mut n.object, o), std::mem::replace(&mut n.in_types, it), std::mem::replace(&mut n.out_types, ot))
    });
    if let Some((oo, oi, oo2)) = old {
        release(c, oo);
        for t in oi.values().chain(oo2.values()) {
            release(c, *t);
        }
    }
    copy_attrs(c, from, b);
    0
}
/// The attributes of `from` copied into `to` (references taken).
fn copy_attrs(c: &mut Ctx, from: u32, to: u32) {
    let s = store_of(from).unwrap_or_default();
    for (_, v) in &s.items {
        if let Val::Unk(u) = v {
            add_ref(c, *u);
        }
    }
    let old = attrs(to, |a| std::mem::replace(&mut a.1, s)).unwrap_or_default();
    for (_, v) in old.items {
        if let Val::Unk(u) = v {
            release(c, u);
        }
    }
}

fn node_vtable() -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    *VT.get_or_init(|| {
        let mut m = attrs_methods();
        m[0] = (tn_qi, 12);
        m[2] = (tn_release, 4);
        m.extend([
            (tn_set_object as Handler, 8),
            (tn_get_object, 8),
            (tn_get_type, 8),
            (tn_get_id, 8),
            (tn_set_id, 12),
            (|c: &mut Ctx| tn_count(c, false), 8),
            (|c: &mut Ctx| tn_count(c, true), 8),
            (tn_connect, 16),
            (tn_disconnect, 8),
            (|c: &mut Ctx| tn_link(c, false), 16),
            (|c: &mut Ctx| tn_link(c, true), 16),
            (|c: &mut Ctx| tn_set_pref(c, true), 12),
            (|c: &mut Ctx| tn_get_pref(c, true), 12),
            (|c: &mut Ctx| tn_set_pref(c, false), 12),
            (|c: &mut Ctx| tn_get_pref(c, false), 12),
            (tn_clone_from, 8),
        ]);
        vtable("IMFTopologyNode", &m)
    })
}
pub fn new_node(kind: u32) -> u32 {
    let b = new_store(node_vtable());
    node_insert(b, Node { kind, id: next_id(), ..Node::default() });
    b
}
fn node_insert(b: u32, n: Node) {
    NODES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, n);
}

/// MFCreateTopologyNode(MF_TOPOLOGY_TYPE, IMFTopologyNode **)
fn MFCreateTopologyNode(c: &mut Ctx) -> u32 {
    let (k, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    if k > TEE {
        putp(out, 0);
        return E_INVALIDARG;
    }
    putp(out, new_node(k));
    0
}

// --- topologies -----------------------------------------------------------------------------

fn tp_qi(c: &mut Ctx) -> u32 {
    // IMFTopology {83cf873a-f6da-4bc8-823f-bacfd55dc433}
    let g = guid_at(c.a(1));
    if is(&g, 0x83cf_873a, 0xf6da, 0x4bc8) && g[15] != 0x33 {
        putp(c.a(2), 0);
        return E_NOINTERFACE;
    }
    reader::typed_qi(c, 0x83cf_873a, 0xf6da, 0x4bc8)
}
fn topo_gone(c: &mut Ctx, b: u32) {
    let Some(t) = TOPOS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&b) else { return };
    for n in t.nodes {
        release(c, n);
    }
}
fn tp_release(c: &mut Ctx) -> u32 {
    store_release(c, topo_gone)
}
fn tp_get_id(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w64(c.a(1), topo(c.a(0), |t| t.id).unwrap_or(0));
    0
}
fn tp_add(c: &mut Ctx) -> u32 {
    let (b, n) = (c.a(0), c.a(1));
    if n == 0 {
        return E_POINTER;
    }
    if node(n, |_| ()).is_none() || topo(b, |t| t.nodes.contains(&n)) != Some(false) {
        return E_INVALIDARG;
    }
    add_ref(c, n);
    topo(b, |t| t.nodes.push(n));
    0
}
fn tp_remove(c: &mut Ctx) -> u32 {
    let (b, n) = (c.a(0), c.a(1));
    let gone = topo(b, |t| t.nodes.iter().position(|&x| x == n).map(|i| t.nodes.remove(i))).flatten();
    match gone {
        Some(n) => {
            release(c, n);
            0
        }
        None => E_INVALIDARG,
    }
}
fn tp_count(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w16(c.a(1), topo(c.a(0), |t| t.nodes.len() as u16).unwrap_or(0));
    0
}
fn tp_get(c: &mut Ctx) -> u32 {
    let (i, out) = (c.a(1) as u16 as usize, c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let n = topo(c.a(0), |t| t.nodes.get(i).copied()).flatten();
    putp(out, n.unwrap_or(0));
    match n {
        Some(n) => {
            add_ref(c, n);
            0
        }
        None => MF_E_INVALIDINDEX,
    }
}
fn tp_clear(c: &mut Ctx) -> u32 {
    let ns = topo(c.a(0), |t| std::mem::take(&mut t.nodes)).unwrap_or_default();
    for n in ns {
        release(c, n);
    }
    0
}
/// CloneFrom(IMFTopology *): new nodes as `from`'s (their ids, objects, types, attributes),
/// connected as they are, and its attributes
fn tp_clone_from(c: &mut Ctx) -> u32 {
    let (b, from) = (c.a(0), c.a(1));
    if from == 0 {
        return E_POINTER;
    }
    let Some(ns) = topo(from, |t| t.nodes.clone()) else { return E_INVALIDARG };
    let mut map = HashMap::new();
    for &n in &ns {
        let Some((kind, id, o, it, ot)) = node(n, |x| (x.kind, x.id, x.object, x.in_types.clone(), x.out_types.clone())) else { continue };
        for &t in it.values().chain(ot.values()) {
            add_ref(c, t);
        }
        add_ref(c, o);
        let m = new_store(node_vtable());
        node_insert(m, Node { kind, id, object: o, in_types: it, out_types: ot, ..Node::default() });
        copy_attrs(c, n, m);
        map.insert(n, m);
    }
    for &n in &ns {
        let outs = node(n, |x| x.outputs.clone()).unwrap_or_default();
        for (i, l) in outs.into_iter().enumerate() {
            let Some((d, di)) = l else { continue };
            let (Some(&m), Some(&dm)) = (map.get(&n), map.get(&d)) else { continue };
            add_ref(c, dm);
            node(m, |x| {
                x.outputs.resize(x.outputs.len().max(i + 1), None);
                x.outputs[i] = Some((dm, di));
            });
            node(dm, |x| {
                x.inputs.resize(x.inputs.len().max(di as usize + 1), None);
                x.inputs[di as usize] = Some((m, i as u32));
            });
        }
    }
    tp_clear(c);
    topo(b, |t| t.nodes = ns.iter().filter_map(|n| map.get(n).copied()).collect());
    copy_attrs(c, from, b);
    0
}
fn tp_by_id(c: &mut Ctx) -> u32 {
    let id = u64_arg(c, 1, 1);
    let out = after64(c, 3);
    if out == 0 {
        return E_POINTER;
    }
    let ns = topo(c.a(0), |t| t.nodes.clone()).unwrap_or_default();
    let n = ns.into_iter().find(|&n| node(n, |x| x.id) == Some(id));
    putp(out, n.unwrap_or(0));
    match n {
        Some(n) => {
            add_ref(c, n);
            0
        }
        None => MF_E_NOT_FOUND,
    }
}
/// The nodes of topology `b` of type `kind`.
pub fn nodes_of(b: u32, kind: u32) -> Vec<u32> {
    let ns = topo(b, |t| t.nodes.clone()).unwrap_or_default();
    ns.into_iter().filter(|&n| node(n, |x| x.kind) == Some(kind)).collect()
}
fn tp_collection(c: &mut Ctx, kind: u32) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    let ns = nodes_of(c.a(0), kind);
    for &n in &ns {
        add_ref(c, n);
    }
    putp(out, new_collection(ns));
    0
}

fn topo_vtable() -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    *VT.get_or_init(|| {
        let mut m = attrs_methods();
        m[0] = (tp_qi, 12);
        m[2] = (tp_release, 4);
        m.extend([
            (tp_get_id as Handler, 8),
            (tp_add, 8),
            (tp_remove, 8),
            (tp_count, 8),
            (tp_get, 12),
            (tp_clear, 4),
            (tp_clone_from, 8),
            (tp_by_id, 16),
            (|c: &mut Ctx| tp_collection(c, SOURCESTREAM), 8),
            (|c: &mut Ctx| tp_collection(c, OUTPUT), 8),
        ]);
        vtable("IMFTopology", &m)
    })
}

/// MFCreateTopology(IMFTopology **)
fn MFCreateTopology(c: &mut Ctx) -> u32 {
    let out = c.a(0);
    if out == 0 {
        return E_POINTER;
    }
    let b = new_store(topo_vtable());
    TOPOS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, Topo { id: next_id(), nodes: Vec::new() });
    putp(out, b);
    0
}

// --- the renderers' activates ---------------------------------------------------------------

fn ac_qi(c: &mut Ctx) -> u32 {
    // IMFActivate {7fee9e9a-4a89-47a6-899c-b6a53a70fb67}
    reader::typed_qi(c, 0x7fee_9e9a, 0x4a89, 0x47a6)
}
fn ac_release(c: &mut Ctx) -> u32 {
    store_release(c, |_, b| {
        ACTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&b);
    })
}
fn activate_vtable() -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    *VT.get_or_init(|| {
        let mut m = attrs_methods();
        m[0] = (ac_qi, 12);
        m[2] = (ac_release, 4);
        // ActivateObject(iid, void **): the sinks are the session's own (it draws and plays
        // what the topology's source gives); ShutdownObject, DetachObject
        m.extend([((|c: &mut Ctx| none(c, 2)) as Handler, 12), (ok, 4), (ok, 4)]);
        vtable("IMFActivate", &m)
    })
}
fn new_activate(a: Act) -> u32 {
    let b = new_store(activate_vtable());
    ACTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, a);
    b
}
/// MFCreateVideoRendererActivate(HWND, IMFActivate **)
fn MFCreateVideoRendererActivate(c: &mut Ctx) -> u32 {
    let (h, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    putp(out, new_activate(Act::Video(h)));
    0
}
/// MFCreateAudioRendererActivate(IMFActivate **)
fn MFCreateAudioRendererActivate(c: &mut Ctx) -> u32 {
    let out = c.a(0);
    if out == 0 {
        return E_POINTER;
    }
    putp(out, new_activate(Act::Audio));
    0
}

// --- collections ----------------------------------------------------------------------------

fn co_qi(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let g = guid_at(c.a(1));
    // IMFCollection {5bc8a76b-869a-46a3-9b03-fa218a66aebe}
    if is_unknown(&g) || is(&g, 0x5bc8_a76b, 0x869a, 0x46a3) {
        coll(this, |s| s.0 += 1);
        putp(out, this);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn co_add_ref(c: &mut Ctx) -> u32 {
    coll(c.a(0), |s| {
        s.0 += 1;
        s.0
    })
    .unwrap_or(1)
}
fn co_release(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let n = coll(b, |s| {
        s.0 = s.0.saturating_sub(1);
        s.0
    })
    .unwrap_or(0);
    if n == 0 {
        if let Some((_, v)) = COLLS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&b) {
            for u in v {
                release(c, u);
            }
            mem::heap_free(b);
        }
    }
    n
}
fn co_count(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), coll(c.a(0), |s| s.1.len() as u32).unwrap_or(0));
    0
}
/// GetElement(DWORD, IUnknown **) / RemoveElement(DWORD, IUnknown **)
fn co_get(c: &mut Ctx, remove: bool) -> u32 {
    let (i, out) = (c.a(1) as usize, c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let e = coll(c.a(0), |s| if i < s.1.len() { Some(if remove { s.1.remove(i) } else { s.1[i] }) } else { None }).flatten();
    let Some(e) = e else {
        putp(out, 0);
        return E_INVALIDARG;
    };
    if !remove {
        add_ref(c, e);
    }
    putp(out, e);
    if e == 0 { E_UNEXPECTED } else { 0 }
}
fn co_add(c: &mut Ctx) -> u32 {
    let e = c.a(1);
    add_ref(c, e);
    coll(c.a(0), |s| s.1.push(e));
    0
}
fn co_insert(c: &mut Ctx) -> u32 {
    let (i, e) = (c.a(1) as usize, c.a(2));
    add_ref(c, e);
    coll(c.a(0), |s| {
        if s.1.len() < i {
            s.1.resize(i, 0);
        }
        s.1.insert(i, e)
    });
    0
}
fn co_clear(c: &mut Ctx) -> u32 {
    let v = coll(c.a(0), |s| std::mem::take(&mut s.1)).unwrap_or_default();
    for u in v {
        release(c, u);
    }
    0
}
/// A collection of `items` (their references handed over).
pub fn new_collection(items: Vec<u32>) -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    let vt = *VT.get_or_init(|| {
        vtable(
            "IMFCollection",
            &[
                (co_qi as Handler, 12),
                (co_add_ref, 4),
                (co_release, 4),
                (co_count, 8),
                (|c: &mut Ctx| co_get(c, false), 12),
                (co_add, 8),
                (|c: &mut Ctx| co_get(c, true), 12),
                (co_insert, 12),
                (co_clear, 4),
            ],
        )
    });
    let b = object(vt, 1);
    COLLS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, (1, items));
    b
}
/// MFCreateCollection(IMFCollection **)
fn MFCreateCollection(c: &mut Ctx) -> u32 {
    let out = c.a(0);
    if out == 0 {
        return E_POINTER;
    }
    putp(out, new_collection(Vec::new()));
    0
}

pub fn init() {
    implement("mf", "MFCreateTopology", MFCreateTopology);
    implement("mf", "MFCreateTopologyNode", MFCreateTopologyNode);
    implement("mf", "MFCreateVideoRendererActivate", MFCreateVideoRendererActivate);
    implement("mf", "MFCreateAudioRendererActivate", MFCreateAudioRendererActivate);
    implement("mfplat", "MFCreateCollection", MFCreateCollection);
}
