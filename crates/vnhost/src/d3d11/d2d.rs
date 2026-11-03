//! What sake's Direct2D needs of a D3D11 texture: its size, a clear, and drawing a block of
//! premultiplied BGRA pixels over it (source over). sake draws text and shapes on the CPU and
//! hands the changed rectangle here at EndDraw; it goes over the texture with a fixed WGSL
//! pipeline (no game shaders, no state of the game's context touched).

use super::*;

// (the block's corner comes as first_instance: y << 16 | x; the staging texture holds the block
// at its top left)
const VS: &str = "struct O { @builtin(position) p: vec4<f32>, @location(0) @interpolate(flat) at: vec2<i32> }
@vertex fn main(@builtin(vertex_index) i: u32, @builtin(instance_index) k: u32) -> O {
    let uv = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    return O(vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0), vec2<i32>(i32(k & 0xffffu), i32(k >> 16u)));
}
";
const PS: &str = "@group(1) @binding(16) var t0: texture_2d<f32>;
@fragment fn main(@builtin(position) p: vec4<f32>, @location(0) @interpolate(flat) at: vec2<i32>) -> @location(0) vec4<f32> {
    return textureLoad(t0, vec2<i32>(p.xy) - at, 0);
}
";

struct D2 {
    vs: u32,
    ps: u32,
    /// the staging texture (object id) and its size
    tex: Option<(u32, u32, u32)>,
}
static D2S: Mutex<Option<D2>> = Mutex::new(None);

/// The texture behind a D3D11 texture or DXGI surface address: (id, width, height).
fn texture(d: &D11, addr: u32) -> Option<(u32, u32, u32)> {
    let id = d.id_of(addr);
    match d.objs.get(&id).map(|o| &o.data) {
        Some(Data::Texture(t)) if t.dim == 2 => Some((id, t.w, t.h)),
        _ => None,
    }
}

/// A texture's (or surface's) size.
pub fn size(addr: u32) -> Option<(u32, u32)> {
    let g = D11S.lock().unwrap_or_else(|e| e.into_inner());
    g.as_ref().and_then(|d| texture(d, addr)).map(|t| (t.1, t.2))
}

/// Fills the whole texture with `rgba` (straight floats).
pub fn clear(addr: u32, rgba: [f32; 4]) {
    let mut g = D11S.lock().unwrap_or_else(|e| e.into_inner());
    let Some(d) = g.as_mut() else { return };
    let Some((id, _, _)) = texture(d, addr) else { return };
    let Some(Data::Texture(t)) = d.objs.get(&id).map(|o| &o.data) else { return };
    let c = rgba.map(f32::to_bits);
    let tex = t.gpu;
    gpu::record(|q| q.cmd(&[gpu::CLEAR_VIEW, tex, 0, 0, 1, c[0], c[1], c[2], c[3], 0, 0]));
    d.targets = None;
}

/// Draws `w`x`h` premultiplied BGRA pixels (`px`, 4 bytes each, rows packed) at (x, y) over the
/// texture, source over. The rectangle must lie inside the texture.
pub fn composite(addr: u32, x: u32, y: u32, w: u32, h: u32, px: &[u8]) {
    if w == 0 || h == 0 || px.len() < (w * h * 4) as usize {
        return;
    }
    let mut g = D11S.lock().unwrap_or_else(|e| e.into_inner());
    let Some(d) = g.as_mut() else { return };
    let Some((target, tw, th)) = texture(d, addr) else { return };
    if x + w > tw || y + h > th {
        return;
    }
    let mut s = D2S.lock().unwrap();
    let s = s.get_or_insert_with(|| {
        let mut sh = [0u32; 2];
        for (i, code) in [VS, PS].iter().enumerate() {
            let id = page_id();
            gpu::record(|q| {
                let off = q.blob(code.as_bytes());
                q.cmd(&[gpu::CREATE_SHADER, id, off, code.len() as u32]);
            });
            sh[i] = id;
        }
        D2 { vs: sh[0], ps: sh[1], tex: None }
    });
    // the staging texture, as large as the largest block so far
    let staging = match s.tex {
        Some((id, sw, sh)) if sw >= w && sh >= h => id,
        old => {
            if let Some((id, _, _)) = old {
                d.release(id);
            }
            let (sw, sh) = (w.max(old.map_or(0, |o| o.1)), h.max(old.map_or(0, |o| o.2)));
            let a = new_texture(d, 2, sw, sh, 1, 1, 87, 0, 8, false);
            let id = d.id_of(a);
            s.tex = Some((id, sw, sh));
            id
        }
    };
    let (Some(Data::Texture(st)), Some(Data::Texture(tt))) = (d.objs.get(&staging).map(|o| &o.data), d.objs.get(&target).map(|o| &o.data)) else { return };
    let (sgpu, tgpu, tpage) = (st.gpu, tt.gpu, tt.page);
    unsafe { upload_texture(st, 0, px.as_ptr() as u32, w * 4, Some([0, 0, 0, w, h, 1])) };
    // pipeline: triangle strip, no culling, no depth, no vertex buffers, one target blended
    // ONE, INV_SRC_ALPHA
    let mut key = vec![s.vs, s.ps, 4, 0, 1, 0, 0, 0, 0, 0xffff, 0, 1];
    key.extend_from_slice(&[tpage, 1, blend_factor(2, false), blend_factor(6, false), 1, blend_factor(2, true), blend_factor(6, true), 1, 0xf]);
    let pipe = match d.pipelines.get(&key) {
        Some(&p) => p,
        None => {
            let id = page_id();
            let mut c = vec![gpu::CREATE_PIPELINE2, id, key.len() as u32];
            c.extend_from_slice(&key);
            gpu::record(|q| q.cmd(&c));
            d.pipelines.insert(key, id);
            id
        }
    };
    gpu::record(|q| q.cmd(&[gpu::SET_TARGETS2, 1, tgpu, 0, 0, 0, 0, 0]));
    d.targets = None;
    let mut c = vec![gpu::DRAW2, pipe];
    c.extend([x as f32, y as f32, w as f32, h as f32, 0.0, 1.0].map(f32::to_bits));
    c.extend_from_slice(&[0, 0, 0, 0, 0]);
    c.extend([0f32; 4].map(f32::to_bits));
    c.extend_from_slice(&[0, 0, 0, 0, 0]);
    // group 0: nothing; group 1: the staging texture (its used corner) and a point sampler
    c.push(0);
    c.push(1);
    c.extend_from_slice(&[16, 1, sgpu, 1, 0, 1, 0, 1]);
    c.extend_from_slice(&[0, 4, 1, 0, 0, y << 16 | x]);
    gpu::record(|q| q.cmd(&c));
}
