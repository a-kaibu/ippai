//! Save states (crates/kura/src/snap.rs, docs/savestate.md): what the GPU holds goes into
//! memory, as one batch that makes it all again. Every texture's levels and layers and every
//! buffer are read back; the batch creates the textures, shaders, pipelines and buffers with
//! the commands that created them and fills them with what was read. A restored module's
//! drawing thread runs it before anything else (render/mod.rs), then the guest's batches go on.
//!
//! Depth and stencil can't be read back (WebGPU): they come back as they were last cleared
//! (games clear them every frame). Multisampled color textures can't be copied either: each
//! sample is drawn into a 1x texture of the format and read back (`samples_out`), and a
//! restored one is drawn sample by sample (multisample mask) from those (`samples_in`).

use super::*;

/// The GPU's part of a state.
pub struct Saved {
    pub c: Vec<u32>,
    pub blob: Vec<u8>,
    /// multisampled textures' samples: id, and per sample (bytes per row, the rows)
    ms: Vec<(u32, Samples)>,
    /// the texture the page showed, where, flipped, in which window
    overlay: Option<(u32, [i32; 4], bool, u32, [f32; 4])>,
}

/// How long a state waits for the GPU's reads: past it the state fails (the game goes on)
/// instead of the page waiting for ever on a read that never comes (a map that never answers).
pub const SNAP_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// A state given up on while reads were on their way: what they write into.
pub struct Grave {
    /// (held only: the page writes into it)
    _blob: Vec<u8>,
    reads: Vec<Read>,
    /// token -> read
    tokens: HashMap<u32, usize>,
}

/// Where `n` bytes go in a blob that has `total` given out: at the next multiple of 4.
fn place(total: &mut usize, n: usize) -> usize {
    let at = total.div_ceil(4) * 4;
    *total = at + n;
    at
}

/// A multisampled texture's samples read back: per sample, bytes per row and the rows.
type Samples = Vec<(u32, Vec<u8>)>;

enum What {
    Tex { id: u32, level: u32, layer: u32, w: u32, h: u32, bpr: u32, halved: bool },
    /// sample `sample` of multisampled `id`, drawn into `tex` (1x) to be read
    Sample { id: u32, sample: u32, bpr: u32, tex: Id },
    Buf { id: u32 },
}

pub(super) struct Read {
    what: What,
    buf: Id,
    /// what was read: bytes of its own (rows that are changed before they are kept: halves,
    /// samples), or `len` bytes at `at` of the state's blob (`D3d::snap_blob`), where they
    /// stay. Every read had a Vec of its own that was then copied into the blob: twice all
    /// the game's textures in memory while a state was taken.
    data: Vec<u8>,
    at: Option<(usize, usize)>,
    /// 0 waiting, 1 read, 2 failed
    state: u8,
}

impl D3d {
    /// Starts reading back every texture and buffer (`snap_ready` says when all came).
    pub fn snap_begin(&mut self) {
        self.end_pass();
        let pending: Vec<u32> = self.clears.keys().copied().collect();
        for t in pending {
            self.settle(t);
        }
        self.end_pass();
        let mut reads = Vec::new();
        // (the blob's bytes given out so far)
        let mut total = 0usize;
        // (and what the halves take once widened to floats, added after them)
        let mut widened = 0usize;
        let mut ids: Vec<u32> = self.textures.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let t = &self.textures[&id];
            if is_depth(t.format) || t.make.is_empty() {
                continue;
            }
            if t.samples > 1 {
                if !t.halved {
                    reads.extend(self.samples_out(id));
                }
                continue;
            }
            let bpp = bpp(t.format);
            for level in 0..t.levels {
                let (w, h) = ((t.w >> level).max(1), (t.h >> level).max(1));
                let layers = if t.dim == "3d" { (t.layers >> level).max(1) } else { t.layers };
                for layer in 0..layers {
                    let bpr = (w * bpp).div_ceil(256) * 256;
                    let buf = gpu::buffer(bpr * h, usage::COPY_DST | usage::MAP_READ);
                    self.open = true;
                    gpu::copy_layer_to_buffer(t.tex, level, [0, 0, layer], buf, bpr, w, h);
                    let n = bpr as usize * h as usize;
                    if t.halved {
                        widened += n * 2 + 4;
                    }
                    let (data, at) = if t.halved { (vec![0; n], None) } else { (Vec::new(), Some((place(&mut total, n), n))) };
                    reads.push(Read { what: What::Tex { id, level, layer, w, h, bpr, halved: t.halved }, buf, data, at, state: 0 });
                }
            }
        }
        let mut ids: Vec<u32> = self.buffers.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let (b, size) = self.buffers[&id];
            let buf = gpu::buffer(size, usage::COPY_DST | usage::MAP_READ);
            self.open = true;
            gpu::copy_buffer(b, buf, size);
            reads.push(Read { what: What::Buf { id }, buf, data: Vec::new(), at: Some((place(&mut total, size as usize), size as usize)), state: 0 });
        }
        self.submit();
        // (room for the shaders' text after them, so that adding it moves nothing)
        let more: usize = widened + 4 + self.shader_src.values().map(|c| c.len() + 4).sum::<usize>();
        let mut blob = Vec::new();
        blob.reserve_exact(total + more);
        blob.resize(total, 0u8);
        for (k, r) in reads.iter_mut().enumerate() {
            let token = self.next_token;
            self.next_token = self.next_token.wrapping_add(1).max(1);
            match r.at {
                Some((at, n)) => gpu::read_buffer(r.buf, n as u32, blob[at..at + n].as_mut_ptr(), token),
                None => gpu::read_buffer(r.buf, r.data.len() as u32, r.data.as_mut_ptr(), token),
            }
            self.snap_tokens.insert(token, k);
        }
        self.snap_blob = blob;
        self.snap_reads = Some(reads);
        self.snap_began = Some(std::time::Instant::now());
    }

    /// A read `snap_begin` asked for came (`mapped`): whether `token` was one.
    pub(super) fn snap_mapped(&mut self, token: u32, ok: bool) -> bool {
        if let Some(g) = self.snap_graves.iter().position(|g| g.tokens.contains_key(&token)) {
            let grave = &mut self.snap_graves[g];
            let k = grave.tokens.remove(&token).unwrap();
            if let Some(r) = grave.reads.get(k) {
                gpu::destroy(r.buf);
                if let What::Sample { tex, .. } = r.what {
                    gpu::destroy(tex);
                }
            }
            if grave.tokens.is_empty() {
                self.snap_graves.swap_remove(g);
            }
            return true;
        }
        let Some(k) = self.snap_tokens.remove(&token) else { return false };
        if let Some(r) = self.snap_reads.as_mut().and_then(|v| v.get_mut(k)) {
            r.state = if ok { 1 } else { 2 };
            gpu::destroy(r.buf);
            if let What::Sample { tex, .. } = r.what {
                gpu::destroy(tex);
            }
        }
        true
    }

    /// Whether everything was read back (and no readback of the guest's is on its way):
    /// Some(all of it read).
    /// Past SNAP_WAIT: Some(false), the reads not come yet given up (`snap_take`).
    pub fn snap_ready(&self) -> Option<bool> {
        let reads = self.snap_reads.as_ref()?;
        if reads.iter().any(|r| r.state == 0) || !self.readbacks.is_empty() {
            return self.snap_began.is_some_and(|t| t.elapsed() >= SNAP_WAIT).then_some(false);
        }
        Some(reads.iter().all(|r| r.state == 1))
    }

    /// The batch that makes it all again (after `snap_ready`).
    pub fn snap_take(&mut self) -> Saved {
        let mut reads = self.snap_reads.take().unwrap_or_default();
        self.snap_began = None;
        let mut blob = std::mem::take(&mut self.snap_blob);
        if !self.snap_tokens.is_empty() {
            // (given up with reads on their way: their memory stays until they come; this
            // state is a failed one)
            let tokens = std::mem::take(&mut self.snap_tokens);
            self.snap_graves.push(Grave { _blob: std::mem::take(&mut blob), reads: std::mem::take(&mut reads), tokens });
        }
        let mut c = Vec::new();
        pad4(&mut blob);
        let mut ids: Vec<u32> = self.textures.keys().copied().collect();
        ids.sort_unstable();
        for &id in &ids {
            let t = &self.textures[&id];
            c.extend_from_slice(&t.make);
            if is_depth(t.format) {
                c.extend(depth_clears(id, t));
            } else if t.samples > 1 {
                c.extend(ms_clears(id));
            }
        }
        let mut ids: Vec<u32> = self.shader_src.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let code = self.shader_src[&id].as_bytes();
            c.extend_from_slice(&[CREATE_SHADER, id, blob.len() as u32, code.len() as u32]);
            blob.extend_from_slice(code);
            pad4(&mut blob);
        }
        let mut ids: Vec<u32> = self.pipe_keys.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let k = &self.pipe_keys[&id];
            c.extend_from_slice(&[CREATE_PIPELINE, id, k.len() as u32]);
            c.extend_from_slice(k);
        }
        let mut ids: Vec<u32> = self.pipelines2.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let k = &self.pipelines2[&id].key;
            c.extend_from_slice(&[CREATE_PIPELINE2, id, k.len() as u32]);
            c.extend_from_slice(k);
        }
        let mut ids: Vec<(u32, u32)> = self.buffers.iter().map(|(&id, &(_, size))| (id, size)).collect();
        ids.sort_unstable();
        for (id, size) in ids {
            c.extend_from_slice(&[CREATE_BUFFER, id, size]);
        }
        let mut ms: Vec<(u32, Samples)> = Vec::new();
        for r in reads.into_iter().filter(|r| r.state == 1) {
            match r.what {
                What::Sample { id, sample, bpr, .. } => {
                    match ms.last_mut() {
                        Some((i, v)) if *i == id && v.len() == sample as usize => v.push((bpr, r.data)),
                        _ if sample == 0 => ms.push((id, vec![(bpr, r.data)])),
                        _ => {}
                    }
                    continue;
                }
                What::Tex { id, level, layer, w, h, bpr, .. } if r.at.is_some() => {
                    c.extend_from_slice(&[WRITE_TEXTURE2, id, level, layer, 0, 0, w, h, bpr, r.at.unwrap().0 as u32]);
                    continue;
                }
                What::Buf { id } if r.at.is_some() => {
                    let (at, n) = r.at.unwrap();
                    c.extend_from_slice(&[WRITE_BUFFER, id, 0, at as u32, n as u32]);
                    continue;
                }
                What::Tex { id, level, layer, w, h, bpr, halved } => {
                    let off = blob.len() as u32;
                    let bpr = if halved {
                        // (kept as halves, written as 32-bit floats)
                        blob.extend(r.data.chunks_exact(2).flat_map(|x| f16_to_f32(u16::from_le_bytes([x[0], x[1]])).to_le_bytes()));
                        bpr * 2
                    } else {
                        blob.extend_from_slice(&r.data);
                        bpr
                    };
                    c.extend_from_slice(&[WRITE_TEXTURE2, id, level, layer, 0, 0, w, h, bpr, off]);
                }
                What::Buf { id } => {
                    c.extend_from_slice(&[WRITE_BUFFER, id, 0, blob.len() as u32, r.data.len() as u32]);
                    blob.extend_from_slice(&r.data);
                }
            }
            pad4(&mut blob);
        }
        // (Direct3D 11's targets last across batches)
        if let Some(t) = &self.targets2 {
            c.push(SET_TARGETS2);
            c.extend_from_slice(t);
        }
        let overlay = self.overlay.and_then(|o| self.textures.contains_key(&self.overlay_tex).then_some((self.overlay_tex, o.rect, o.flip, o.hwnd, o.src)));
        // (all four samples, or none: the texture comes back as zeros)
        ms.retain(|(_, v)| v.len() == 4);
        Saved { c, blob, ms, overlay }
    }

    /// A fresh D3d (a restored module's): makes what `s` holds.
    pub fn snap_restore(&mut self, s: &Saved) {
        self.run_batch(&s.c, &[], &s.blob);
        for (id, samples) in &s.ms {
            self.samples_in(*id, samples);
        }
        if let Some((id, rect, flip, hwnd, src)) = s.overlay {
            if self.textures.contains_key(&id) {
                // (a multisampled one shows its resolve: of zeros, until the game presents again)
                let view = self.present_view(id);
                self.overlay = Some(Overlay { view, rect, flip, hwnd, src });
                self.overlay_tex = id;
                self.tex_gen += 1;
            }
        }
    }
}

impl D3d {
    /// The GPU was lost (host/vngpu.js, render/mod.rs `vnhost_render_lost`): nothing on it can be
    /// read back any more. The guest's readbacks on their way end without pixels (it stops
    /// waiting), and the batch returned makes every object again on a new device as `snap_take`
    /// does, empty: textures as zeros, depth as last cleared, multisampled ones as zeros. The
    /// guest uploads again what it still has (d3d9, d3d11: `render::lost_gen`).
    pub fn lose(&mut self) -> Saved {
        let mut reqs: Vec<u32> = self.readbacks.drain().map(|(_, r)| r.req).collect();
        reqs.sort_unstable();
        for req in reqs {
            vnhost_d3d9_readback_done(req);
        }
        self.checks.clear();
        self.check_done.clear();
        self.snap_tokens.clear();
        self.snap_reads = None;
        // (a lost device's maps fail: nothing writes into the graves any more)
        self.snap_graves.clear();
        self.snap_take()
    }

    /// The D3d that goes on on a new device after this one's was lost (`lose`): every object
    /// made again, empty, the clears not done yet kept. The old table of objects is forgotten
    /// (gpu::reset) before anything is made on the new device.
    pub fn renew(mut self) -> D3d {
        let saved = self.lose();
        let clears = self.take_clears();
        let frames = self.frames;
        // (its objects are the old device's: forgotten with the table, not dropped one by one)
        drop(self);
        gpu::reset();
        let mut d = D3d::new();
        d.snap_restore(&saved);
        d.keep_clears(clears);
        d.frames = frames;
        d
    }

    /// The clears not done yet (by texture: they name no GPU object), for the D3d that goes on
    /// after a lost device: what they clear comes back as they would leave it.
    fn take_clears(&mut self) -> HashMap<u32, Clear> {
        std::mem::take(&mut self.clears)
    }
    /// (over the depth clears `lose`'s batch made: a clear not done yet is newer)
    fn keep_clears(&mut self, c: HashMap<u32, Clear>) {
        for (id, c) in c {
            if self.textures.contains_key(&id) {
                self.clears.insert(id, c);
            }
        }
    }
}

/// The sample type of a color format: WGSL's and the bind group layout's.
fn sample_kind(format: &str) -> (&'static str, &'static str) {
    if format.ends_with("uint") {
        ("u32", "uint")
    } else if format.ends_with("sint") {
        ("i32", "sint")
    } else {
        ("f32", "unfilterable-float")
    }
}

/// The sample copy shader: sample `sample` of a multisampled texture (None: the texel of a 1x
/// one) at the fragment's pixel, of WGSL type `ty`.
pub(crate) fn sample_copy_shader(ty: &str, sample: Option<u32>) -> String {
    let (src, load) = match sample {
        Some(k) => (format!("texture_multisampled_2d<{ty}>"), format!("{k}")),
        None => (format!("texture_2d<{ty}>"), "0".into()),
    };
    format!(
        "{SAMPLE_COPY}@group(0) @binding(0) var src: {src};
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {{
  let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
  return vec4f(p * 2.0 - 1.0, 0.0, 1.0);
}}
@fragment fn fs(@builtin(position) p: vec4f) -> @location(0) vec4<{ty}> {{ return textureLoad(src, vec2<i32>(p.xy), {load}); }}
"
    )
}

impl D3d {
    /// A pipeline drawing the sample copy into `format` with `samples` and multisample `mask`;
    /// its bind group layout (the caller drops both).
    fn sample_copy_pipe(&mut self, format: &'static str, sample: Option<u32>, samples: u32, mask: u32) -> (Id, Id, Id) {
        let (ty, st) = sample_kind(format);
        let m = gpu::shader(&sample_copy_shader(ty, sample));
        let layout = gpu::bind_group_layout(&format!(
            r#"{{"entries":[{{"binding":0,"visibility":2,"texture":{{"sampleType":{},"viewDimension":"2d","multisampled":{}}}}}]}}"#,
            q(st),
            sample.is_some()
        ));
        let pl = gpu::pipeline_layout(&format!(r#"{{"bindGroupLayouts":[{layout}]}}"#));
        let p = gpu::render_pipeline(&format!(
            r#"{{"layout":{pl},"vertex":{{"module":{m},"entryPoint":"vs"}},"fragment":{{"module":{m},"entryPoint":"fs","targets":[{{"format":{}}}]}},"primitive":{{"topology":"triangle-list"}},"multisample":{{"count":{samples},"mask":{mask}}}}}"#,
            q(format)
        ));
        gpu::drop(pl);
        (p, layout, m)
    }

    /// Multisampled `id`'s four samples drawn into 1x textures and read back (snap_begin's).
    fn samples_out(&mut self, id: u32) -> Vec<Read> {
        let t = &self.textures[&id];
        let (w, h, format, view) = (t.w, t.h, t.format, t.view);
        let bpr = (w * bpp(format)).div_ceil(256) * 256;
        let mut reads = Vec::new();
        self.end_pass();
        for sample in 0..4 {
            let (pipe, layout, module) = self.sample_copy_pipe(format, Some(sample), 1, 0xf);
            let tex = gpu::texture(&format!(r#"{{"size":[{w},{h}],"format":{},"usage":{}}}"#, q(format), usage::T_RENDER | usage::T_COPY_SRC));
            let tv = gpu::view(tex, "");
            let group = gpu::bind_group(layout, &[(0, Res::Obj(view))]);
            self.open = true;
            gpu::begin_pass(&format!(r#"{{"colorAttachments":[{{"view":{tv},"loadOp":"clear","storeOp":"store"}}]}}"#));
            gpu::set_pipeline(pipe);
            gpu::set_bind_group(0, group, &[]);
            gpu::draw(3, 1, 0, 0);
            gpu::end_pass();
            let buf = gpu::buffer(bpr * h, usage::COPY_DST | usage::MAP_READ);
            gpu::copy_layer_to_buffer(tex, 0, [0, 0, 0], buf, bpr, w, h);
            [group, tv, pipe, layout, module].into_iter().for_each(gpu::drop);
            reads.push(Read { what: What::Sample { id, sample, bpr, tex }, buf, data: vec![0; (bpr * h) as usize], at: None, state: 0 });
        }
        reads
    }

    /// Multisampled `id` (made again, its clears done) filled from its samples' rows: each drawn
    /// into the samples of one bit of the mask.
    fn samples_in(&mut self, id: u32, samples: &[(u32, Vec<u8>)]) {
        let Some(t) = self.textures.get(&id) else { return };
        if t.samples != 4 || samples.len() != 4 {
            return;
        }
        let (w, h, format) = (t.w, t.h, t.format);
        self.settle(id);
        self.end_pass();
        let target = self.textures[&id].view;
        for (k, (bpr, rows)) in samples.iter().enumerate() {
            let tex = gpu::texture(&format!(r#"{{"size":[{w},{h}],"format":{},"usage":{}}}"#, q(format), usage::T_BINDING | usage::T_COPY_DST));
            gpu::write_texture(tex, 0, [0, 0, 0], rows.as_ptr(), rows.len(), *bpr, h, [w, h, 1]);
            let tv = gpu::view(tex, "");
            let (pipe, layout, module) = self.sample_copy_pipe(format, None, 4, 1 << k);
            let group = gpu::bind_group(layout, &[(0, Res::Obj(tv))]);
            self.open = true;
            gpu::begin_pass(&format!(r#"{{"colorAttachments":[{{"view":{target},"loadOp":"load","storeOp":"store"}}]}}"#));
            gpu::set_pipeline(pipe);
            gpu::set_bind_group(0, group, &[]);
            gpu::draw(3, 1, 0, 0);
            gpu::end_pass();
            [group, tv, pipe, layout, module].into_iter().for_each(gpu::drop);
            self.doomed.push(tex);
        }
        self.use_tex(id);
        self.submit();
    }
}

/// What a depth-stencil texture comes back as: its last clear (or depth 1, as a new one).
pub(super) fn depth_clears(id: u32, t: &Tex) -> Vec<u32> {
    let (z, s) = t.zclear.unwrap_or((1.0, 0));
    let layers = if t.dim == "3d" { 1 } else { t.layers };
    (0..t.levels).flat_map(|level| (0..layers).flat_map(move |layer| [CLEAR_VIEW, id, level, layer, 6, 0, 0, 0, 0, z.to_bits(), s])).collect()
}

/// What a multisampled color texture comes back as before its samples are drawn (or if they
/// could not be read): zeros, as a new one, made explicit so that what restores it is the same
/// command whatever the page did before.
pub(super) fn ms_clears(id: u32) -> Vec<u32> {
    vec![CLEAR_VIEW, id, 0, 0, 1, 0, 0, 0, 0, 0, 0]
}

fn pad4(b: &mut Vec<u8>) {
    b.resize(b.len().div_ceil(4) * 4, 0);
}
