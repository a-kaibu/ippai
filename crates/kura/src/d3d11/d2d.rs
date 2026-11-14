//! What sake's Direct2D needs of a D3D11 texture: its size, a clear, and drawing its shapes on
//! the GPU. sake works out where each shape goes on the CPU (transforms, clips, text layout) and
//! hands a list of [`Item`]s here at EndDraw; one instanced draw paints them over the texture,
//! source over, with a fixed WGSL pipeline (no game shaders, no state of the game's context
//! touched). Glyph coverage lives in an R8 atlas and gradients in a ramp texture (256 texels a
//! row), both kept here and uploaded before the draw that reads them.

use super::*;

const DATA: u32 = 0xffff_ffff;

/// `Item::kind[0]`: coverage of the pixel by `rect` (partial at its edges)
pub const RECT: u32 = 0;
/// coverage by the ellipse `shape` (center, outer radii) minus, when `extra[2]` != 0, the one
/// with radii `extra[0..2]`, 4x4 samples a pixel inside `rect` (`extra[3]` != 0: aliased, the
/// pixel's center only)
pub const ELLIPSE: u32 = 1;
/// coverage from the glyph atlas at pixel + `shape[0..2]`
pub const GLYPH: u32 = 2;
/// the draw's bitmap (premultiplied): destination rectangle `shape` (device pixels) showing
/// its texels `extra` (l, t, r, b), times `paint[0]` (opacity); `paint[1]` 1: linear (else
/// nearest), `paint[2]` 1: its alpha ignored. Partial coverage at the edges of `rect`.
pub const BITMAP: u32 = 3;
/// coverage by a transformed unit square (or, `extra[3]` != 0, the circle inscribed in it):
/// `shape` (2x2) and `extra[0..2]` map device positions to its (u, v); `extra[2]` != 0:
/// aliased (the pixel's center only), else 4x4 samples; inside `rect`
pub const QUAD: u32 = 4;

/// One shape. Device pixels; `rect` is already inside the target and the clips.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Item {
    pub rect: [f32; 4],
    pub shape: [f32; 4],
    pub extra: [f32; 4],
    /// straight RGBA; with a ramp: (start x, y, direction / length²)
    pub paint: [f32; 4],
    /// (kind, ramp row + 1 or 0, set by `draw`: target width, height)
    pub kind: [u32; 4],
}

const VS: &str = "struct V {
    @builtin(position) p: vec4<f32>,
    @location(0) @interpolate(flat) rect: vec4<f32>,
    @location(1) @interpolate(flat) shape: vec4<f32>,
    @location(2) @interpolate(flat) extra: vec4<f32>,
    @location(3) @interpolate(flat) paint: vec4<f32>,
    @location(4) @interpolate(flat) kind: vec4<u32>,
}
@vertex fn main(@builtin(vertex_index) i: u32, @location(0) rect: vec4<f32>, @location(1) shape: vec4<f32>,
        @location(2) extra: vec4<f32>, @location(3) paint: vec4<f32>, @location(4) kind: vec4<u32>) -> V {
    let uv = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    let c = mix(floor(rect.xy), ceil(rect.zw), uv);
    let size = vec2<f32>(f32(kind.z), f32(kind.w));
    return V(vec4<f32>(c.x / size.x * 2.0 - 1.0, 1.0 - c.y / size.y * 2.0, 0.0, 1.0), rect, shape, extra, paint, kind);
}
";
const PS: &str = "@group(1) @binding(16) var atlas: texture_2d<f32>;
@group(1) @binding(17) var ramps: texture_2d<f32>;
// bitmap {
@group(1) @binding(18) var bmp: texture_2d<f32>;
fn texel(q: vec2<i32>, lo: vec2<i32>, hi: vec2<i32>, opaque: bool) -> vec4<f32> {
    let c = textureLoad(bmp, clamp(q, lo, hi), 0);
    return select(c, vec4<f32>(c.rgb, 1.0), opaque);
}
// }
@fragment fn main(@builtin(position) p: vec4<f32>,
        @location(0) @interpolate(flat) r: vec4<f32>,
        @location(1) @interpolate(flat) shape: vec4<f32>,
        @location(2) @interpolate(flat) extra: vec4<f32>,
        @location(3) @interpolate(flat) paint: vec4<f32>,
        @location(4) @interpolate(flat) kind: vec4<u32>) -> @location(0) vec4<f32> {
    let px = floor(p.xy);
    var cov = 0.0;
    switch kind.x {
        case 0u: {
            let c = clamp(min(r.zw, px + 1.0) - max(r.xy, px), vec2<f32>(0.0), vec2<f32>(1.0));
            cov = c.x * c.y;
        }
        case 1u: {
            var n = 0u;
            let m = select(16u, 1u, extra.w != 0.0);
            for (var k = 0u; k < m; k++) {
                let s = select(px + vec2<f32>(f32(k % 4u), f32(k / 4u)) * 0.25 + 0.125, px + 0.5, m == 1u);
                let d = s - shape.xy;
                let o = d / shape.zw;
                if (all(s >= r.xy) && all(s < r.zw) && dot(o, o) < 1.0) {
                    let ii = d / extra.xy;
                    if (extra.z == 0.0 || extra.x <= 0.0 || extra.y <= 0.0 || dot(ii, ii) > 1.0) {
                        n++;
                    }
                }
            }
            cov = f32(n) / f32(m);
        }
        case 4u: {
            var n = 0u;
            let m = select(16u, 1u, extra.z != 0.0);
            for (var k = 0u; k < m; k++) {
                let s = select(px + vec2<f32>(f32(k % 4u), f32(k / 4u)) * 0.25 + 0.125, px + 0.5, m == 1u);
                let uv = vec2<f32>(shape.x * s.x + shape.z * s.y + extra.x, shape.y * s.x + shape.w * s.y + extra.y);
                let o = uv * 2.0 - 1.0;
                let inq = select(all(uv >= vec2<f32>(0.0)) && all(uv < vec2<f32>(1.0)), dot(o, o) < 1.0, extra.w != 0.0);
                if (all(s >= r.xy) && all(s < r.zw) && inq) {
                    n++;
                }
            }
            cov = f32(n) / f32(m);
        }
        // bitmap {
        case 3u: {
            let c = clamp(min(r.zw, px + 1.0) - max(r.xy, px), vec2<f32>(0.0), vec2<f32>(1.0));
            // the texel position of the pixel's center
            let t = (px + 0.5 - shape.xy) / max(shape.zw - shape.xy, vec2<f32>(1e-6));
            let s = extra.xy + t * (extra.zw - extra.xy);
            let lo = vec2<i32>(floor(extra.xy));
            let hi = max(vec2<i32>(ceil(extra.zw)) - 1, lo);
            let opaque = paint.z != 0.0;
            var col: vec4<f32>;
            if (paint.y != 0.0) {
                let b = s - 0.5;
                let q = vec2<i32>(floor(b));
                let f = b - floor(b);
                let top = mix(texel(q, lo, hi, opaque), texel(q + vec2<i32>(1, 0), lo, hi, opaque), f.x);
                let bot = mix(texel(q + vec2<i32>(0, 1), lo, hi, opaque), texel(q + vec2<i32>(1, 1), lo, hi, opaque), f.x);
                col = mix(top, bot, f.y);
            } else {
                col = texel(vec2<i32>(floor(s)), lo, hi, opaque);
            }
            return col * (paint.x * c.x * c.y);
        }
        // }
        default: {
            cov = textureLoad(atlas, vec2<i32>(px + shape.xy), 0).r;
        }
    }
    var col = paint;
    if (kind.y != 0u) {
        let t = clamp(dot(px + 0.5 - paint.xy, paint.zw), 0.0, 1.0);
        col = textureLoad(ramps, vec2<i32>(i32(t * 255.0 + 0.5), i32(kind.y - 1u)), 0);
    }
    let a = col.a * cov;
    return vec4<f32>(col.rgb * a, a);
}
";

const ATLAS: u32 = 1024;
const RAMP_ROWS: u32 = 64;

/// A texture kept on the CPU too; the changed rows go up before a draw.
struct Shadow {
    gpu: u32,
    w: u32,
    bpp: u32,
    px: Vec<u8>,
    /// changed rows [y0, y1)
    dirty: Option<(u32, u32)>,
}

impl Shadow {
    fn new(w: u32, h: u32, page: u32, bpp: u32) -> Shadow {
        let gpu = page_id();
        gpu::record(|q| q.cmd(&[gpu::CREATE_TEXTURE2, gpu, w, h, 1, 1, page, 0]));
        Shadow { gpu, w, bpp, px: vec![0; (w * h * bpp) as usize], dirty: None }
    }

    fn put(&mut self, x: u32, y: u32, w: u32, h: u32, src: &[u8]) {
        let row = (w * self.bpp) as usize;
        for k in 0..h {
            let at = (((y + k) * self.w + x) * self.bpp) as usize;
            self.px[at..at + row].copy_from_slice(&src[k as usize * row..][..row]);
        }
        self.dirty = Some(self.dirty.map_or((y, y + h), |(a, b)| (a.min(y), b.max(y + h))));
    }

    fn upload(&mut self) {
        let Some((y0, y1)) = self.dirty.take() else { return };
        let bpr = self.w * self.bpp;
        let rows = &self.px[(y0 * bpr) as usize..(y1 * bpr) as usize];
        let (gpu, w) = (self.gpu, self.w);
        gpu::record(|q| {
            let off = q.blob(rows);
            q.cmd(&[gpu::WRITE_TEXTURE2, gpu, 0, 0, 0, y0, w, y1 - y0, bpr, off]);
        });
    }
}

/// `PS` without its `// bitmap {` .. `// }` parts: the draws without a bitmap (a smaller
/// shader, which the software rasterizers of weak devices run faster)
fn plain_ps() -> String {
    let mut out = String::new();
    let mut skip = false;
    for l in PS.lines() {
        match l.trim() {
            "// bitmap {" => skip = true,
            "// }" if skip => skip = false,
            _ if !skip => {
                out.push_str(l);
                out.push('\n');
            }
            _ => {}
        }
    }
    out
}

struct D2 {
    vs: u32,
    /// the shader for draws without a bitmap, with one
    ps: u32,
    ps_bitmap: u32,
    atlas: Shadow,
    /// glyph key -> atlas position
    glyphs: HashMap<u64, [u32; 2]>,
    /// the shelf being filled: x, y, height
    shelf: [u32; 3],
    ramps: Shadow,
    /// ramp contents -> row
    rows: HashMap<Vec<u8>, u32>,
}
static D2S: Mutex<Option<D2>> = Mutex::new(None);

fn d2<R>(f: impl FnOnce(&mut D2) -> R) -> R {
    let mut g = D2S.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| {
        let mut sh = [0u32; 3];
        for (i, code) in [VS, &plain_ps(), PS].iter().enumerate() {
            let id = page_id();
            gpu::record(|q| {
                let off = q.blob(code.as_bytes());
                q.cmd(&[gpu::CREATE_SHADER, id, off, code.len() as u32]);
            });
            sh[i] = id;
        }
        D2 {
            vs: sh[0],
            ps: sh[1],
            ps_bitmap: sh[2],
            atlas: Shadow::new(ATLAS, ATLAS, format::P_R8, 1),
            glyphs: HashMap::new(),
            shelf: [0; 3],
            ramps: Shadow::new(256, RAMP_ROWS, format::P_RGBA8, 4),
            rows: HashMap::new(),
        }
    }))
}

/// Where glyph `key` (`w`x`h` coverage bytes `cov`, made only when it is new) is in the atlas;
/// None: the atlas is full (draw what uses it, then `reset`).
pub fn glyph(key: u64, w: u32, h: u32, cov: impl FnOnce() -> Vec<u8>) -> Option<[u32; 2]> {
    d2(|s| {
        if let Some(&at) = s.glyphs.get(&key) {
            return Some(at);
        }
        if w > ATLAS || h > ATLAS {
            return Some([0, 0]);
        }
        let [mut x, mut y, mut sh] = s.shelf;
        if x + w > ATLAS {
            (x, y, sh) = (0, y + sh, 0);
        }
        if y + h > ATLAS {
            return None;
        }
        let px = cov();
        if px.len() >= (w * h) as usize {
            s.atlas.put(x, y, w, h, &px);
        }
        s.shelf = [x + w + 1, y, sh.max(h + 1)];
        s.glyphs.insert(key, [x, y]);
        Some([x, y])
    })
}

/// The ramp row (straight RGBA8, 256 texels from t = 0 to 1) holding `texels`; None: no room
/// (draw what uses the ramps, then `reset`).
pub fn ramp(texels: &[u8; 1024]) -> Option<u32> {
    d2(|s| {
        if let Some(&r) = s.rows.get(&texels[..]) {
            return Some(r);
        }
        let r = s.rows.len() as u32;
        if r >= RAMP_ROWS {
            return None;
        }
        s.ramps.put(0, r, 256, 1, texels);
        s.rows.insert(texels.to_vec(), r);
        Some(r)
    })
}

/// Forgets the atlas and the ramps (after the items that used them were drawn).
pub fn reset() {
    d2(|s| {
        s.glyphs.clear();
        s.shelf = [0; 3];
        s.rows.clear();
    })
}

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

/// The GPU texture behind a D3D11 texture or DXGI surface address (to draw it as a bitmap).
pub fn texture_gpu(addr: u32) -> Option<u32> {
    let g = D11S.lock().unwrap_or_else(|e| e.into_inner());
    let d = g.as_ref()?;
    let (id, _, _) = texture(d, addr)?;
    match d.objs.get(&id).map(|o| &o.data) {
        Some(Data::Texture(t)) => Some(t.gpu),
        _ => None,
    }
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

/// A texture to draw into, for a D2D bitmap made with D2D1_BITMAP_OPTIONS_TARGET (CreateBitmap):
/// B8G8R8A8, render target and shader resource, `w` x `h`; its address, as a D3D11 texture's
/// (None before a D3D11 device). KiriKiri's DrawDeviceD2D draws its frame into one, then that
/// onto the swap chain's buffer.
pub fn target_texture(w: u32, h: u32) -> Option<u32> {
    let mut g = D11S.lock().unwrap_or_else(|e| e.into_inner());
    let d = g.as_mut()?;
    Some(super::new_texture(d, 2, w.max(1), h.max(1), 1, 1, 87, 0, 0x28, false, 1))
}

/// A bitmap of its own (BGRA, premultiplied): its texture id.
pub fn bitmap(w: u32, h: u32) -> u32 {
    let id = page_id();
    gpu::record(|q| q.cmd(&[gpu::CREATE_TEXTURE2, id, w.max(1), h.max(1), 1, 1, format::P_BGRA8, 0]));
    id
}

/// Writes rows `px` (`pitch` bytes apart) to bitmap `id` at (x, y), `w` x `h`.
pub fn bitmap_write(id: u32, (x, y, w, h): (u32, u32, u32, u32), px: &[u8], pitch: u32) {
    let row = (w * 4) as usize;
    let all = row * h as usize;
    // (rows back to back already: the caller's bytes straight into the queue, one copy, not two)
    let packed = pitch as usize == row && px.len() >= all;
    let mut rows = Vec::new();
    if !packed {
        rows.reserve(all);
        for k in 0..h as usize {
            match px.get(k * pitch as usize..k * pitch as usize + row) {
                Some(r) => rows.extend_from_slice(r),
                None => return,
            }
        }
    }
    let bytes = if packed { &px[..all] } else { &rows[..] };
    gpu::record(|q| {
        let off = q.blob(bytes);
        q.cmd(&[gpu::WRITE_TEXTURE2, id, 0, 0, x, y, w, h, w * 4, off]);
    });
}

/// Frees a bitmap's texture.
pub fn bitmap_free(id: u32) {
    gpu::record(|q| q.cmd(&[gpu::DESTROY_TEXTURE, id]));
}

/// Paints `items` over the texture in order, source over (premultiplied); BITMAP items show
/// texture `bitmap`.
pub fn draw(addr: u32, items: &[Item], bitmap: Option<u32>) {
    if items.is_empty() {
        return;
    }
    let mut g = D11S.lock().unwrap_or_else(|e| e.into_inner());
    let Some(d) = g.as_mut() else { return };
    let Some((target, tw, th)) = texture(d, addr) else { return };
    let Some(Data::Texture(tt)) = d.objs.get(&target).map(|o| &o.data) else { return };
    let (tgpu, tpage) = (tt.gpu, tt.page);
    let (vs, ps, atlas, ramps) = d2(|s| {
        s.atlas.upload();
        s.ramps.upload();
        (s.vs, if bitmap.is_some() { s.ps_bitmap } else { s.ps }, s.atlas.gpu, s.ramps.gpu)
    });
    // pipeline: triangle strip, no culling, no depth, one instance buffer, one target blended
    // ONE, INV_SRC_ALPHA
    let mut key = vec![vs, ps, 4, 0, 1, 0, 0, 0, 0, 0xffff];
    key.extend_from_slice(&[1, std::mem::size_of::<Item>() as u32, 1, 5, 3, 0, 0, 3, 16, 1, 3, 32, 2, 3, 48, 3, 7, 64, 4]);
    key.extend_from_slice(&[1, tpage, 1, blend_factor(2, false), blend_factor(6, false), 1, blend_factor(2, true), blend_factor(6, true), 1, 0xf]);
    let pipe = match d.pipelines.get(&key) {
        Some(&p) => {
            crate::render::gpu::pipeline_hit();
            p
        }
        None => {
            let id = page_id();
            let mut c = vec![gpu::CREATE_PIPELINE2, id, key.len() as u32];
            c.extend_from_slice(&key);
            gpu::record(|q| q.cmd(&c));
            d.pipelines.insert(key, id);
            id
        }
    };
    let mut bytes = Vec::with_capacity(items.len() * std::mem::size_of::<Item>());
    for it in items {
        let mut it = *it;
        (it.kind[2], it.kind[3]) = (tw, th);
        for v in it.rect.iter().chain(&it.shape).chain(&it.extra).chain(&it.paint) {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for v in it.kind {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    d.targets = None;
    gpu::record(|q| {
        q.cmd(&[gpu::SET_TARGETS2, 1, tgpu, 0, 0, 0, 0, 0]);
        let vo = q.data(&bytes, 16);
        let mut c = vec![gpu::DRAW2, pipe];
        c.extend([0.0, 0.0, tw as f32, th as f32, 0.0, 1.0].map(f32::to_bits));
        c.extend_from_slice(&[0, 0, 0, 0, 0]);
        c.extend([0f32; 4].map(f32::to_bits));
        // stencil reference, the instance buffer, no index buffer
        c.extend_from_slice(&[0, 1, DATA, vo, 0, 0, 0]);
        // group 0: nothing; group 1: the atlas, the ramps (and the bitmap)
        c.push(0);
        c.push(2 + bitmap.is_some() as u32);
        c.extend_from_slice(&[16, 1, atlas, 1, 0, 1, 0, 1]);
        c.extend_from_slice(&[17, 1, ramps, 1, 0, 1, 0, 1]);
        if let Some(b) = bitmap {
            c.extend_from_slice(&[18, 1, b, 1, 0, 1, 0, 1]);
        }
        c.extend_from_slice(&[0, 4, items.len() as u32, 0, 0, 0, 0]);
        q.cmd(&c);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig { cases: 64, ..Default::default() })]

        /// bitmap_write of any box with any pitch (rows back to back go to the queue as they
        /// are, others row by row) on the model of WebGPU: the bitmap holds the rows, the rest
        /// stays as it was; a source too short for its rows writes nothing.
        #[test]
        fn bitmap_writes_its_rows(x in 0u32..6, y in 0u32..6, w in 1u32..7, h in 1u32..7, extra in proptest::prop_oneof![proptest::strategy::Just(0u32), 1u32..9], short: bool, seed: u64) {
            let _g = crate::d3d9::trace_tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
            crate::d3d9::trace_tests::attach_page();
            let id = bitmap(12, 12);
            let pitch = w * 4 + extra;
            let mut k = seed | 1;
            let mut px: Vec<u8> = (0..pitch * (h - 1) + w * 4).map(|_| { k ^= k << 13; k ^= k >> 7; k ^= k << 17; k as u8 }).collect();
            if short {
                px.pop();
            }
            bitmap_write(id, (x, y, w, h), &px, pitch);
            gpu::pump();
            // (the readback's memory where 32-bit addresses reach it)
            let rb_at = unsafe { crate::d3d9::trace_tests::alloc(12 * 12 * 4) };
            unsafe { std::ptr::write_bytes(rb_at, 0, 12 * 12 * 4) };
            let req = 0x5300_0000 | (seed as u32 & 0xffff);
            gpu::record(|q| q.cmd(&[gpu::READBACK, req, id, 0, 0, 0, 12, 12, rb_at as u32, 48]));
            proptest::prop_assert!(gpu::wait_readback(req));
            let rb = unsafe { std::slice::from_raw_parts(rb_at, 12 * 12 * 4) };
            bitmap_free(id);
            gpu::set_pump(None);
            for ty in 0..12u32 {
                for tx in 0..12u32 {
                    let at = ((ty * 12 + tx) * 4) as usize;
                    let inside = !short && tx >= x && tx < x + w && ty >= y && ty < y + h;
                    let want = if inside { let s = ((ty - y) * pitch + (tx - x) * 4) as usize; px[s..s + 4].to_vec() } else { vec![0; 4] };
                    proptest::prop_assert_eq!(&rb[at..at + 4], &want[..], "texel ({}, {})", tx, ty);
                }
            }
            let errors = crate::render::model::with(|m| std::mem::take(&mut m.errors));
            proptest::prop_assert!(errors.is_empty(), "WebGPU errors {:?}", errors);
        }
    }

    #[test]
    fn shaders_validate() {
        let plain = super::plain_ps();
        assert!(!plain.contains("bmp") && !plain.contains("// }"));
        for (code, what) in [(super::VS, "vs"), (super::PS, "ps"), (plain.as_str(), "plain ps")] {
            let m = naga::front::wgsl::parse_str(code).unwrap_or_else(|e| panic!("{what}: {}", e.emit_to_string(code)));
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all()).validate(&m).unwrap_or_else(|e| panic!("{what}: {e:?}"));
        }
    }
}
