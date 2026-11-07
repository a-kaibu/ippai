//! Deferred contexts and command lists (CreateDeferredContext, FinishCommandList,
//! ExecuteCommandList), as Windows has them:
//!
//! - a deferred context has its own state, which its Set* calls change and its Get* calls
//!   read; drawing, clearing, copying and updating on it only record;
//! - a command list starts from the state the deferred context had when the list began (the
//!   default one, or what FinishCommandList(TRUE) kept), then replays its calls in order;
//! - ExecuteCommandList(list, restore) runs it on the immediate context from that starting state
//!   (nothing of the immediate context's state comes in), then gives back the immediate
//!   context's state (restore) or clears it; on a deferred context it is recorded, and the
//!   deferred context's state is cleared unless restore;
//! - Map on a deferred context: only WRITE_DISCARD of dynamic resources (then WRITE_NO_OVERWRITE
//!   of a buffer already discarded in the list); what is written is recorded at Unmap.
//!
//! A recorded call keeps copies of what its pointers point to, and private references to the
//! objects it names, until the list goes.

use super::*;

/// D3D11_ERROR_DEFERRED_CONTEXT_MAP_WITHOUT_INITIAL_DISCARD
const MAP_WITHOUT_DISCARD: u32 = 0x887c_0004;

/// The most a recorded call copies from one pointer (an UpdateSubresource of 256 MB)
const MAX_BLOB: u64 = 1 << 28;

pub struct Rec {
    m: u32,
    args: [u32; 16],
    /// (argument, the bytes it pointed to)
    blobs: Vec<(usize, Box<[u8]>)>,
}

pub struct CmdList {
    /// the state the list starts from (held privately)
    start: State,
    /// (shared: a list executed runs its calls where they are, without copying them)
    recs: std::sync::Arc<Vec<Rec>>,
    /// objects the calls name (held privately)
    held: Vec<u32>,
}

pub struct Deferred {
    st: State,
    list: CmdList,
    /// mapped here: (resource, subresource, scratch memory, size, row pitch, depth pitch)
    maps: Vec<(u32, u32, u32, u32, u32, u32)>,
}

/// The objects a state holds.
pub(super) fn state_objects(s: &State) -> Vec<u32> {
    let mut held: Vec<u32> = Vec::new();
    held.extend(s.vbs.iter().map(|v| v.0));
    held.extend([s.ib.0, s.layout, s.vs, s.ps, s.dsv, s.blend, s.depth, s.raster, s.predicate.0]);
    held.extend(s.shaders);
    held.extend(s.uavs.iter().flatten());
    held.extend(s.so);
    for k in 0..6 {
        held.extend(s.cbs[k]);
        held.extend(s.srvs[k]);
        held.extend(s.samplers[k]);
    }
    held.extend(s.rtvs);
    held.retain(|&h| h != 0);
    held
}

/// A copy of `s` holding its own references.
fn clone_state(d: &mut D11, s: &State) -> State {
    for h in state_objects(s) {
        d.bind(h);
    }
    s.clone()
}

fn new_list(d: &mut D11, start: &State) -> CmdList {
    CmdList { start: clone_state(d, start), recs: Default::default(), held: Vec::new() }
}

/// Lets go of what a list holds.
pub(super) fn drop_list(d: &mut D11, l: CmdList) {
    unbind_state(d, &l.start);
    for h in l.held {
        d.unbind(h);
    }
}

pub(super) fn drop_deferred(d: &mut D11, x: Deferred) {
    unbind_state(d, &x.st);
    drop_list(d, x.list);
    for m in x.maps {
        d.guest_free(m.2);
    }
}

/// CreateDeferredContext(flags, out) (and ...1, ...2)
pub(super) unsafe fn create(d: &mut D11, flags: u32, out: u32) -> u32 {
    if out == 0 {
        return E_INVALIDARG;
    }
    if flags != 0 {
        wrp(out, 0, 0);
        return E_INVALIDARG;
    }
    let st = default_state();
    let list = new_list(d, &st);
    let o = d.new_obj(IF_CONTEXT, Data::Deferred(Box::new(Deferred { st, list, maps: Vec::new() })));
    // (a device child: it holds the device while it is held)
    let dev = d.device;
    d.add_ref(dev);
    wrp(out, 0, o);
    S_OK
}

/// What a call's pointers point to: (argument, bytes); None: more than it can be (a size
/// that does not fit, or over MAX_BLOB)
unsafe fn pointed(d: &D11, m: u32, a: &dyn Fn(usize) -> u32) -> Option<Vec<(usize, u32)>> {
    let ps = ptr();
    let n = |i: usize| a(i).min(128);
    Some(match m {
        // *Set{ConstantBuffers, ShaderResources, Samplers}(start, n, objects)
        7 | 8 | 10 | 16 | 22 | 25 | 26 | 31 | 32 | 59 | 61 | 62 | 63 | 65 | 66 | 67 | 70 | 71 => vec![(3, n(2) * ps)],
        // *SetShader(shader, class instances, n)
        9 | 11 | 23 | 60 | 64 | 69 => vec![(2, n(3) * ps)],
        18 => vec![(3, n(2) * ps), (4, 4 * n(2)), (5, 4 * n(2))],
        33 => vec![(2, n(1).min(8) * ps)],
        34 => {
            let keep = |v: u32| if v == 0xffff_ffff { 0 } else { v.min(8) };
            vec![(2, keep(a(1)) * ps), (6, keep(a(5)) * ps), (7, 4 * keep(a(5)))]
        }
        35 | 50 | 51 | 52 => vec![(2, 16)],
        37 => vec![(2, n(1).min(4) * ps), (3, 4 * n(1).min(4))],
        44 => vec![(2, 24 * n(1))],
        45 => vec![(2, 16 * n(1))],
        46 | 115 => vec![(8, 24)],
        48 | 116 => vec![(3, 24), (4, update_size(d, a)?)],
        68 => vec![(3, n(2) * ps), (4, 4 * n(2))],
        119..=124 => vec![(3, n(2) * ps), (4, 4 * n(2)), (5, 4 * n(2))],
        132 => vec![(2, 16), (3, 16 * n(4))],
        _ => vec![],
    })
}

/// The bytes UpdateSubresource(resource, sub, box, data, row pitch, depth pitch) reads: as
/// update_subresource and upload_texture read them (a texture's box from its block's corner).
unsafe fn update_size(d: &D11, a: &dyn Fn(usize) -> u32) -> Option<u32> {
    let bx = (a(3) != 0).then(|| [rd(a(3), 0), rd(a(3), 1), rd(a(3), 2), rd(a(3), 3), rd(a(3), 4), rd(a(3), 5)]);
    let size: u64 = match d.objs.get(&d.id_of(a(1))).map(|o| &o.data) {
        Some(Data::Buffer(b)) => bx.map_or(b.size, |x| x[3].min(b.size).saturating_sub(x[0])) as u64,
        Some(Data::Texture(t)) => {
            let level = a(2) % t.mips.max(1);
            let (lw, lh) = ((t.w >> level).max(1), (t.h >> level).max(1));
            let ld = if t.dim == 3 { (t.layers >> level).max(1) } else { 1 };
            let block = format::info(t.fmt).map_or(1, |f| f.block.max(1));
            let [x0, y0, z0, x1, y1, z1] = bx.unwrap_or([0, 0, 0, lw, lh, ld]);
            let (x0, y0) = (x0 / block * block, y0 / block * block);
            let (w, h, dp) = (x1.min(lw).saturating_sub(x0), y1.min(lh).saturating_sub(y0), z1.min(ld).saturating_sub(z0));
            if w == 0 || h == 0 || dp == 0 {
                return Some(0);
            }
            let (row, rows) = format::pitch_rows(t.fmt, w, h);
            // (in 64 bits: the pitches are the application's)
            (dp as u64 - 1) * a(6) as u64 + (rows as u64 - 1) * a(5) as u64 + row as u64
        }
        _ => 0,
    };
    (size <= MAX_BLOB && (a(4) as u64) + size <= 1 << 32).then_some(size as u32)
}

/// A call onto the list being recorded (nobody else has it yet).
fn push(l: &mut CmdList, r: Rec) {
    if let Some(v) = std::sync::Arc::get_mut(&mut l.recs) {
        v.push(r);
    }
}

/// Records a call (copies of what it points to, references to what it names).
unsafe fn record(d: &mut D11, this: u32, m: u32, a: &dyn Fn(usize) -> u32) {
    // (only the arguments the method has: the caller's array is no longer than that)
    let mut args = [0u32; 16];
    args.copy_from_slice(&super::args_of(IF_CONTEXT, m, a, 16));
    let Some(ptrs) = pointed(d, m, a) else {
        // (data past what any resource takes, or past the address space: not recorded)
        if d.trace {
            eprintln!("d3d11: deferred call {m} with data too large to record, dropped");
        }
        return;
    };
    let mut blobs = Vec::new();
    for (i, size) in ptrs {
        if a(i) != 0 && size != 0 {
            blobs.push((i, Box::from(std::slice::from_raw_parts(a(i) as *const u8, size as usize))));
        }
    }
    hold(d, this, m, &args, &blobs);
    if let Some(Data::Deferred(x)) = d.objs.get_mut(&this).map(|o| &mut o.data) {
        push(&mut x.list, Rec { m, args, blobs });
    }
}

/// Which arguments of a call are objects, and which of its pointers point to arrays of them.
fn objects(m: u32) -> (&'static [usize], &'static [usize]) {
    match m {
        7 | 8 | 10 | 16 | 22 | 25 | 26 | 31 | 32 | 59 | 61 | 62 | 63 | 65 | 66 | 67 | 70 | 71 | 18 | 68 | 119..=124 => (&[], &[3]),
        9 | 11 | 23 | 60 | 64 | 69 => (&[1], &[2]),
        33 => (&[3], &[2]),
        34 => (&[3], &[2, 6]),
        37 => (&[], &[2]),
        17 | 19 | 27 | 28 | 30 | 35 | 36 | 39 | 40 | 42 | 43 | 48 | 50..=55 | 58 | 116..=118 | 132 | 133 => (&[1], &[]),
        46 | 115 => (&[1, 6], &[]),
        47 => (&[1, 2], &[]),
        49 | 57 => (&[1, 3], &[]),
        _ => (&[], &[]),
    }
}

/// The objects a recorded call names, held by the list being recorded.
fn hold(d: &mut D11, this: u32, m: u32, args: &[u32; 16], blobs: &[(usize, Box<[u8]>)]) {
    let ps = ptr() as usize;
    let (singles, arrays) = objects(m);
    let mut ids: Vec<u32> = singles.iter().map(|&i| d.id_of(args[i])).collect();
    for (i, b) in blobs {
        if arrays.contains(i) {
            ids.extend(b.chunks_exact(ps).map(|c| d.id_of(u32::from_le_bytes([c[0], c[1], c[2], c[3]]))));
        }
    }
    ids.retain(|&i| i != 0 && i != this);
    for &i in &ids {
        d.bind(i);
    }
    if let Some(Data::Deferred(x)) = d.objs.get_mut(&this).map(|o| &mut o.data) {
        x.list.held.extend(ids);
    }
}

/// Takes the deferred context's data out (to work on it with `d`), and puts it back.
fn with_deferred<R>(d: &mut D11, this: u32, f: impl FnOnce(&mut D11, &mut Deferred) -> R) -> Option<R> {
    let o = d.objs.get_mut(&this)?;
    let Data::Deferred(x) = std::mem::replace(&mut o.data, Data::Other) else {
        return None;
    };
    let mut x = x;
    let r = f(d, &mut x);
    if let Some(o) = d.objs.get_mut(&this) {
        o.data = Data::Deferred(x);
    }
    Some(r)
}

/// A method of a deferred context.
pub(super) unsafe fn call(d: &mut D11, this: u32, m: u32, a: &dyn Fn(usize) -> u32, after: &mut After) -> u32 {
    match m {
        3 => {
            let dev = d.device;
            d.add_ref(dev);
            wrp(a(1), 0, d.addr_of(dev));
            S_OK
        }
        4..=6 => private_data(d, m, this, a),
        // state: set (and recorded), read
        7..=11 | 16..=19 | 22..=26 | 30..=37 | 43..=45 | 59..=71 | 110 | 119..=124 => {
            let r = with_deferred(d, this, |d, x| context_call2(d, &mut x.st, m, a, after)).unwrap_or(E_FAIL);
            record(d, this, m, a);
            r
        }
        72..=109 | 125..=130 => with_deferred(d, this, |d, x| context_call2(d, &mut x.st, m, a, after)).unwrap_or(E_FAIL),
        14 => map(d, this, a(1), a(2), a(3), a(5)),
        15 => {
            unmap(d, this, a(1), a(2));
            S_OK
        }
        // queries' data is the immediate context's to give
        29 => DXGI_ERROR_INVALID_CALL,
        58 => {
            // ExecuteCommandList(list, restore): recorded; the state is cleared unless restore
            if !matches!(d.objs.get(&d.id_of(a(1))).map(|o| &o.data), Some(Data::CmdList(_))) {
                return S_OK;
            }
            record(d, this, m, a);
            if a(2) == 0 {
                with_deferred(d, this, |d, x| {
                    let old = std::mem::replace(&mut x.st, default_state());
                    unbind_state(d, &old);
                });
            }
            S_OK
        }
        111 => S_OK,
        112 => 1, // D3D11_DEVICE_CONTEXT_DEFERRED
        113 => 0,
        114 => finish(d, this, a(1), a(2)),
        131 => {
            // SwapDeviceContextState: not on a deferred context
            if a(2) != 0 {
                wrp(a(2), 0, 0);
            }
            S_OK
        }
        // drawing, clearing, copying, updating, queries' Begin/End: recorded
        12 | 13 | 20 | 21 | 27 | 28 | 38..=42 | 46..=57 | 115..=118 | 132 | 133 => {
            record(d, this, m, a);
            S_OK
        }
        134..=139 | 141..=143 => S_OK,
        140 => 0,
        // Flush1 / hardware protection on a deferred context: nothing to do
        144 | 145 => 0,
        146 => {
            if a(1) != 0 {
                wr(a(1), 0, 0);
            }
            0
        }
        147 | 148 => E_INVALIDARG,
        _ => E_NOTIMPL,
    }
}

/// FinishCommandList(restore, out): the list so far; the state kept (restore) or cleared, and
/// the next list starting from it.
unsafe fn finish(d: &mut D11, this: u32, restore: u32, out: u32) -> u32 {
    if out == 0 {
        return E_INVALIDARG;
    }
    let list = with_deferred(d, this, |d, x| {
        // (what a map left open is not in it)
        for m in std::mem::take(&mut x.maps) {
            d.guest_free(m.2);
        }
        if restore == 0 {
            let old = std::mem::replace(&mut x.st, default_state());
            unbind_state(d, &old);
        }
        let next = new_list(d, &x.st);
        std::mem::replace(&mut x.list, next)
    });
    let Some(list) = list else { return E_FAIL };
    let o = d.new_obj(IF_CMDLIST, Data::CmdList(Box::new(list)));
    wrp(out, 0, o);
    S_OK
}

/// ExecuteCommandList(list, restore) on the immediate context (`st` its state).
pub(super) unsafe fn execute(d: &mut D11, st: &mut State, list: u32, restore: u32, after: &mut After) {
    let id = d.id_of(list);
    let Some(Data::CmdList(l)) = d.objs.get(&id).map(|o| &o.data) else { return };
    let (start, recs) = (l.start.clone(), l.recs.clone());
    // (the list is held while it runs: a call in it may let go of the last reference)
    d.bind(id);
    let start = clone_state(d, &start);
    let saved = std::mem::replace(st, start);
    for r in recs.iter() {
        replay(d, st, r, after);
    }
    if restore != 0 {
        let ran = std::mem::replace(st, saved);
        unbind_state(d, &ran);
    } else {
        let ran = std::mem::replace(st, default_state());
        unbind_state(d, &ran);
        unbind_state(d, &saved);
    }
    d.unbind(id);
}

/// One recorded call, on `st`: its copies put back in guest memory for it.
unsafe fn replay(d: &mut D11, st: &mut State, r: &Rec, after: &mut After) {
    let mut args = r.args;
    let mut mem = Vec::new();
    for (i, b) in &r.blobs {
        let p = d.guest_alloc(b.len() as u32);
        std::ptr::copy_nonoverlapping(b.as_ptr(), p as *mut u8, b.len());
        args[*i] = p;
        mem.push(p);
    }
    // (an update reads no more than was copied for it: its box is the copy's)
    let a = |i: usize| args[i];
    let fits = !matches!(r.m, 48 | 116) || update_size(d, &a).is_some_and(|n| n <= r.blobs.iter().find(|b| b.0 == 4).map_or(0, |b| b.1.len() as u32));
    if fits {
        context_call2(d, st, r.m, &a, after);
    }
    for p in mem {
        d.guest_free(p);
    }
}

/// Map(resource, sub, type, flags, out) on a deferred context.
unsafe fn map(d: &mut D11, this: u32, res: u32, sub: u32, ty: u32, out: u32) -> u32 {
    let id = d.id_of(res);
    let (usage, buffer, size, pitch, depth) = match d.objs.get(&id).map(|o| &o.data) {
        Some(Data::Buffer(b)) => (b.usage, true, b.size, b.size, b.size),
        Some(Data::Texture(t)) => {
            let level = sub % t.mips.max(1);
            let (lw, lh) = ((t.w >> level).max(1), (t.h >> level).max(1));
            let (pitch, rows) = format::pitch_rows(t.fmt, lw, lh);
            let ld = if t.dim == 3 { (t.layers >> level).max(1) } else { 1 };
            // (in 64 bits: a 2048^3 texture's level 0 does not fit in 32)
            let size = pitch as u64 * rows as u64 * ld as u64;
            if size > MAX_BLOB {
                if out != 0 {
                    wrp(out, 0, 0);
                }
                return E_OUTOFMEMORY;
            }
            (t.usage, false, size as u32, pitch, pitch * rows)
        }
        _ => (0, false, 0, 0, 0),
    };
    let fail = |hr: u32| {
        if out != 0 {
            wrp(out, 0, 0);
        }
        hr
    };
    if usage != 2 || !(ty == 4 || ty == 5 && buffer) || out == 0 {
        return fail(E_INVALIDARG);
    }
    let r = with_deferred(d, this, |d, x| {
        let have = x.maps.iter().find(|m| m.0 == id && m.1 == sub).map(|m| m.2);
        match (ty, have) {
            (5, None) => Err(MAP_WITHOUT_DISCARD),
            (_, Some(mem)) => Ok(mem),
            (_, None) => {
                let mem = d.guest_alloc(size);
                x.maps.push((id, sub, mem, size, pitch, depth));
                Ok(mem)
            }
        }
    });
    match r {
        Some(Ok(mem)) => {
            put_mapped(out, mem, pitch, depth, false);
            S_OK
        }
        Some(Err(hr)) => fail(hr),
        None => fail(E_FAIL),
    }
}

/// Unmap on a deferred context: what was written, recorded as an update of the subresource.
unsafe fn unmap(d: &mut D11, this: u32, res: u32, sub: u32) {
    let id = d.id_of(res);
    let Some(Some((mem, size, pitch, depth))) = with_deferred(d, this, |_, x| x.maps.iter().find(|m| m.0 == id && m.1 == sub).map(|m| (m.2, m.3, m.4, m.5))) else {
        // (not mapped here: nothing, as Windows does)
        return;
    };
    let args = [0, res, sub, 0, 0, pitch, depth, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let blobs: Vec<(usize, Box<[u8]>)> = vec![(4, Box::from(std::slice::from_raw_parts(mem as *const u8, size as usize)))];
    hold(d, this, 48, &args, &blobs);
    if let Some(Data::Deferred(x)) = d.objs.get_mut(&this).map(|o| &mut o.data) {
        push(&mut x.list, Rec { m: 48, args, blobs });
    }
}

/// (tests: the calls recorded so far on a deferred context, the bytes they copied)
#[cfg(test)]
pub(super) fn recorded(d: &D11, this: u32) -> (usize, usize) {
    match d.objs.get(&this).map(|o| &o.data) {
        Some(Data::Deferred(x)) => (x.list.recs.len(), x.list.recs.iter().flat_map(|r| r.blobs.iter()).map(|b| b.1.len()).sum()),
        _ => (0, 0),
    }
}
