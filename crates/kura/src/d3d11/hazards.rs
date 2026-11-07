//! What the Direct3D 11 runtime does with a resource bound for reading and writing at once
//! (formal/tla/Binds11.tla, winref probe d3d11bind): binding it for output (a render target, a
//! writable depth view, an unordered access view, a stream output target) unbinds the shader
//! resource views (every stage) and vertex buffers that read it; binding it for input while it
//! is bound for output binds NULL instead. Per subresource: views of other mips or array
//! slices of the same texture do not conflict. (On WebGPU a texture sampled and rendered in
//! the same pass is a validation error that loses the pass.)

use super::{Data, State, View, D11};

/// What a view covers: (resource, first mip, mips, first slice, slices); a buffer view covers
/// its whole buffer.
fn span(d: &D11, view: u32) -> Option<(u32, u32, u32, u32, u32)> {
    match d.objs.get(&view).map(|o| &o.data) {
        Some(Data::View(View { buf: Some(b), .. })) => Some((b.buf, 0, u32::MAX, 0, u32::MAX)),
        Some(Data::View(v)) => Some((v.res, v.mip, v.mips.max(1), v.layer, v.layers.max(1))),
        _ => None,
    }
}

fn overlap(a: (u32, u32, u32, u32, u32), b: (u32, u32, u32, u32, u32)) -> bool {
    let meet = |s0: u32, n0: u32, s1: u32, n1: u32| {
        s0 < s1.saturating_add(n1) && s1 < s0.saturating_add(n0)
    };
    a.0 == b.0 && meet(a.1, a.2, b.1, b.2) && meet(a.3, a.4, b.3, b.4)
}

/// The views bound for output: render targets, the depth view unless read-only for depth,
/// the output merger's and the compute shader's UAVs.
fn outputs(d: &D11, st: &State) -> Vec<(u32, u32, u32, u32, u32)> {
    let mut v: Vec<u32> = st
        .rtvs
        .iter()
        .chain(st.uavs[0].iter())
        .chain(st.uavs[1].iter())
        .copied()
        .collect();
    let dsv_ro = match d.objs.get(&st.dsv).map(|o| &o.data) {
        Some(Data::View(x)) => x.desc.get(2).is_some_and(|f| f & 1 != 0),
        _ => false,
    };
    if !dsv_ro {
        v.push(st.dsv);
    }
    v.into_iter()
        .filter(|&x| x != 0)
        .filter_map(|x| span(d, x))
        .collect()
}

/// An input view that must not be bound now (it reads what is bound for output).
fn refused(d: &D11, st: &State, srv: u32) -> bool {
    span(d, srv).is_some_and(|s| outputs(d, st).into_iter().any(|o| overlap(o, s)))
}

/// After SRVs were set in `stage` from `start` (`n` slots): the ones that read an output are
/// NULL.
pub fn after_srvs(d: &mut D11, st: &mut State, stage: usize, start: u32, n: u32) {
    for s in start..start.saturating_add(n).min(128) {
        let v = st.srvs[stage][s as usize];
        if v != 0 && refused(d, st, v) {
            st.srvs[stage][s as usize] = 0;
            d.unbind(v);
        }
    }
}

/// After outputs were set: the SRVs of every stage that read them go.
pub fn after_outputs(d: &mut D11, st: &mut State) {
    let outs = outputs(d, st);
    if outs.is_empty() {
        return;
    }
    for stage in 0..6 {
        for s in 0..128 {
            let v = st.srvs[stage][s];
            if v != 0 && span(d, v).is_some_and(|x| outs.iter().any(|o| overlap(*o, x))) {
                st.srvs[stage][s] = 0;
                d.unbind(v);
            }
        }
    }
}

/// Stream output targets and vertex buffers of the same buffer: the vertex buffer goes (set
/// first, it is unbound; set after, it is refused).
pub fn after_buffers(d: &mut D11, st: &mut State) {
    for s in 0..16 {
        let vb = st.vbs[s].0;
        if vb != 0 && st.so.contains(&vb) {
            st.vbs[s].0 = 0;
            d.unbind(vb);
        }
    }
}
