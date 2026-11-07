//! Save states (crates/kura/src/snap.rs, docs/savestate.md): what the GPU holds goes into
//! memory, as one batch that makes it all again. Every texture's levels and layers and every
//! buffer are read back; the batch creates the textures, shaders, pipelines and buffers with
//! the commands that created them and fills them with what was read. A restored module's
//! drawing thread runs it before anything else (render/mod.rs), then the guest's batches go on.
//!
//! Depth and stencil can't be read back (WebGPU): they come back as they were last cleared
//! (games clear them every frame). Neither can multisampled textures: they come back
//! cleared to zeros (games draw and resolve them every frame).

use super::*;

/// The GPU's part of a state.
pub struct Saved {
    pub c: Vec<u32>,
    pub blob: Vec<u8>,
    /// the texture the page showed, where, flipped
    overlay: Option<(u32, [i32; 4], bool)>,
}

enum What {
    Tex { id: u32, level: u32, layer: u32, w: u32, h: u32, bpr: u32, halved: bool },
    Buf { id: u32 },
}

pub(super) struct Read {
    what: What,
    buf: Id,
    data: Vec<u8>,
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
        let mut ids: Vec<u32> = self.textures.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let t = &self.textures[&id];
            if is_depth(t.format) || t.samples > 1 || t.make.is_empty() {
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
                    reads.push(Read { what: What::Tex { id, level, layer, w, h, bpr, halved: t.halved }, buf, data: vec![0; (bpr * h) as usize], state: 0 });
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
            reads.push(Read { what: What::Buf { id }, buf, data: vec![0; size as usize], state: 0 });
        }
        self.submit();
        for (k, r) in reads.iter_mut().enumerate() {
            let token = self.next_token;
            self.next_token = self.next_token.wrapping_add(1).max(1);
            gpu::read_buffer(r.buf, r.data.len() as u32, r.data.as_mut_ptr(), token);
            self.snap_tokens.insert(token, k);
        }
        self.snap_reads = Some(reads);
    }

    /// A read `snap_begin` asked for came (`mapped`): whether `token` was one.
    pub(super) fn snap_mapped(&mut self, token: u32, ok: bool) -> bool {
        let Some(k) = self.snap_tokens.remove(&token) else { return false };
        if let Some(r) = self.snap_reads.as_mut().and_then(|v| v.get_mut(k)) {
            r.state = if ok { 1 } else { 2 };
            gpu::destroy(r.buf);
        }
        true
    }

    /// Whether everything was read back (and no readback of the guest's is on its way):
    /// Some(all of it read).
    pub fn snap_ready(&self) -> Option<bool> {
        let reads = self.snap_reads.as_ref()?;
        if reads.iter().any(|r| r.state == 0) || !self.readbacks.is_empty() {
            return None;
        }
        Some(reads.iter().all(|r| r.state == 1))
    }

    /// The batch that makes it all again (after `snap_ready`).
    pub fn snap_take(&mut self) -> Saved {
        let reads = self.snap_reads.take().unwrap_or_default();
        self.snap_tokens.clear();
        let mut c = Vec::new();
        let mut blob = Vec::new();
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
        for r in reads.iter().filter(|r| r.state == 1) {
            match r.what {
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
        let overlay = self.overlay.and_then(|o| self.textures.contains_key(&self.overlay_tex).then_some((self.overlay_tex, o.rect, o.flip)));
        Saved { c, blob, overlay }
    }

    /// A fresh D3d (a restored module's): makes what `s` holds.
    pub fn snap_restore(&mut self, s: &Saved) {
        self.run_batch(&s.c, &[], &s.blob);
        if let Some((id, rect, flip)) = s.overlay {
            if self.textures.contains_key(&id) {
                // (a multisampled one shows its resolve: of zeros, until the game presents again)
                let view = self.present_view(id);
                self.overlay = Some(Overlay { view, rect, flip });
                self.overlay_tex = id;
                self.tex_gen += 1;
            }
        }
    }
}

/// What a depth-stencil texture comes back as: its last clear (or depth 1, as a new one).
pub(super) fn depth_clears(id: u32, t: &Tex) -> Vec<u32> {
    let (z, s) = t.zclear.unwrap_or((1.0, 0));
    let layers = if t.dim == "3d" { 1 } else { t.layers };
    (0..t.levels).flat_map(|level| (0..layers).flat_map(move |layer| [CLEAR_VIEW, id, level, layer, 6, 0, 0, 0, 0, z.to_bits(), s])).collect()
}

/// What a multisampled color texture comes back as (not read back): zeros, as a new one, made
/// explicit so that what restores it is the same command whatever the page did before.
pub(super) fn ms_clears(id: u32) -> Vec<u32> {
    vec![CLEAR_VIEW, id, 0, 0, 1, 0, 0, 0, 0, 0, 0]
}

fn pad4(b: &mut Vec<u8>) {
    b.resize(b.len().div_ceil(4) * 4, 0);
}
