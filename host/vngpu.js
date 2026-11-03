// WebGPU for crates/kura/src/render: the thread that draws (one pthread, holding the page's
// canvas as an OffscreenCanvas) calls these. Objects live in a table and go to Rust as indexes;
// descriptors of rare objects come as JSON (numbers under REFS keys are such indexes), the
// per-draw calls as plain arguments. Everything else (what to draw, caches, Direct3D) is Rust.
addToLibrary({
  $VG: {
    objs: [null],
    free: [],
    device: null,
    ctx: null,
    enc: null,
    pass: null,
    put(o) {
      const i = VG.free.length ? VG.free.pop() : VG.objs.length;
      VG.objs[i] = o;
      return i;
    },
    // JSON keys whose numbers are objects of the table
    REFS: { module: 1, layout: 1, view: 1, buffer: 1, texture: 1, sampler: 1, resolveTarget: 1 },
    json(ptr, len) {
      const s = UTF8ToString(ptr >>> 0, len);
      return JSON.parse(s, function (k, v) {
        if (typeof v === 'number' && VG.REFS[k]) return VG.objs[v];
        if (k === 'bindGroupLayouts') return v.map((i) => VG.objs[i]);
        // a bind group entry's resource: {view: n} / {sampler: n} are the object itself
        if (k === 'resource' && v && !v.buffer) return v.view ?? v.sampler;
        return v;
      });
    },
    heap: () => new Uint8Array(wasmMemory.buffer),
    call: (name, ...a) => wasmExports[name](...a),
  },

  // (on the page) sends its canvas to the worker of thread `t` (host/pre.js)
  vngpu_attach__proxy: 'sync',
  vngpu_attach__deps: ['$PThread'],
  vngpu_attach: (t) => {
    const c = Module.vnvmCanvas;
    if (!c) return 0;
    PThread.pthreads[t >>> 0].postMessage({ vnvmCanvas: c }, [c]);
    Module.vnvmCanvas = null;
    return 1;
  },

  // The canvas and a device: then vnhost_render_frame every animation frame.
  vngpu_start__deps: ['$VG'],
  vngpu_start: () => {
    (async () => {
      const canvas = await vnvmCanvas;
      const adapter = await navigator.gpu?.requestAdapter();
      if (!adapter) throw new Error('no WebGPU adapter');
      const features = ['rg11b10ufloat-renderable'].filter((f) => adapter.features.has(f));
      VG.device = await adapter.requestDevice({ requiredFeatures: features });
      VG.device.addEventListener('uncapturederror', (e) => err(`webgpu: ${e.error.message}`));
      VG.canvas = canvas;
      VG.ctx = canvas.getContext('webgpu');
      VG.ctx.configure({ device: VG.device, format: navigator.gpu.getPreferredCanvasFormat(), alphaMode: 'opaque' });
      const frame = (t) => {
        VG.call('vnhost_render_frame', t);
        requestAnimationFrame(frame);
      };
      requestAnimationFrame(frame);
    })().catch((e) => err(`render: ${e}`));
  },

  // the canvas's preferred format: 0 bgra8unorm, 1 rgba8unorm
  vngpu_canvas_format: () => (navigator.gpu.getPreferredCanvasFormat() === 'rgba8unorm' ? 1 : 0),
  vngpu_canvas_size: (w, h) => {
    if (VG.canvas.width !== w) VG.canvas.width = w;
    if (VG.canvas.height !== h) VG.canvas.height = h;
  },
  vngpu_canvas_view: () => VG.put(VG.ctx.getCurrentTexture().createView()),
  vngpu_has_feature: (ptr, len) => (VG.device.features.has(UTF8ToString(ptr >>> 0, len)) ? 1 : 0),

  // objects
  vngpu_drop: (i) => {
    if (i) {
      VG.objs[i] = null;
      VG.free.push(i);
    }
  },
  vngpu_destroy: (i) => {
    VG.objs[i]?.destroy?.();
  },
  vngpu_texture: (ptr, len) => VG.put(VG.device.createTexture(VG.json(ptr, len))),
  vngpu_view: (tex, ptr, len) => VG.put(VG.objs[tex].createView(len ? VG.json(ptr, len) : undefined)),
  vngpu_buffer: (size, usage) => VG.put(VG.device.createBuffer({ size, usage })),
  vngpu_sampler: (ptr, len) => VG.put(VG.device.createSampler(VG.json(ptr, len))),
  vngpu_shader: (ptr, len) => {
    const code = UTF8ToString(ptr >>> 0, len);
    const m = VG.device.createShaderModule({ code });
    m.getCompilationInfo().then((info) => {
      const e = info.messages.filter((m) => m.type === 'error');
      if (e.length) err(`webgpu: shader: ${e.map((m) => `${m.lineNum}: ${m.message}`).join('; ')}\n${code}`);
    });
    return VG.put(m);
  },
  vngpu_bind_group_layout: (ptr, len) => VG.put(VG.device.createBindGroupLayout(VG.json(ptr, len))),
  vngpu_pipeline_layout: (ptr, len) => VG.put(VG.device.createPipelineLayout(VG.json(ptr, len))),
  vngpu_render_pipeline: (ptr, len) => VG.put(VG.device.createRenderPipeline(VG.json(ptr, len))),
  vngpu_pipeline_group_layout: (p, i) => VG.put(VG.objs[p].getBindGroupLayout(i)),
  // entries: 5 words each (binding, kind 0 buffer / 1 view or sampler, object, offset, size)
  vngpu_bind_group: (layout, ptr, n) => {
    const w = new Uint32Array(wasmMemory.buffer, ptr >>> 0, n * 5);
    const entries = [];
    for (let e = 0; e < n * 5; e += 5) {
      const o = VG.objs[w[e + 2]];
      entries.push({ binding: w[e], resource: w[e + 1] ? o : { buffer: o, offset: w[e + 3], size: w[e + 4] } });
    }
    return VG.put(VG.device.createBindGroup({ layout: VG.objs[layout], entries }));
  },

  // the queue (views of the shared memory as they are: WebGPU takes them)
  vngpu_write_buffer: (buf, at, ptr, len) => {
    VG.device.queue.writeBuffer(VG.objs[buf], at, VG.heap(), ptr >>> 0, len);
  },
  vngpu_write_texture: (tex, mip, x, y, z, ptr, len, bpr, rows, w, h, d) => {
    VG.device.queue.writeTexture(
      { texture: VG.objs[tex], mipLevel: mip, origin: { x, y, z } },
      new Uint8Array(wasmMemory.buffer, ptr >>> 0, len),
      { bytesPerRow: bpr, rowsPerImage: rows },
      [w, h, d],
    );
  },
  vngpu_submit: () => {
    if (VG.enc) {
      VG.device.queue.submit([VG.enc.finish()]);
      VG.enc = null;
    }
  },
  // vnhost_render_done(token) once the GPU finished what was submitted
  vngpu_on_done: (token) => {
    const t0 = performance.now();
    VG.device.queue.onSubmittedWorkDone().then(() => VG.call('vnhost_render_done', token, performance.now() - t0));
  },
  // copies buffer `buf` (mapped for reading) to `dst`, then vnhost_render_mapped(token, ok)
  vngpu_read_buffer: (buf, size, dst, token) => {
    const b = VG.objs[buf];
    b.mapAsync(GPUMapMode.READ).then(
      () => {
        new Uint8Array(wasmMemory.buffer).set(new Uint8Array(b.getMappedRange(0, size)), dst >>> 0);
        b.unmap();
        VG.call('vnhost_render_mapped', token, 1);
      },
      () => VG.call('vnhost_render_mapped', token, 0),
    );
  },

  // the command encoder (one, made when needed, ended by vngpu_submit)
  vngpu_copy_texture: (src, smip, sx, sy, sz, dst, dmip, dx, dy, dz, w, h, d) => {
    VG.enc ??= VG.device.createCommandEncoder();
    VG.enc.copyTextureToTexture(
      { texture: VG.objs[src], mipLevel: smip, origin: { x: sx, y: sy, z: sz } },
      { texture: VG.objs[dst], mipLevel: dmip, origin: { x: dx, y: dy, z: dz } },
      [w, h, d],
    );
  },
  vngpu_copy_to_buffer: (tex, mip, x, y, buf, bpr, w, h) => {
    VG.enc ??= VG.device.createCommandEncoder();
    VG.enc.copyTextureToBuffer({ texture: VG.objs[tex], mipLevel: mip, origin: { x, y } }, { buffer: VG.objs[buf], bytesPerRow: bpr }, [w, h]);
  },
  vngpu_begin_pass: (ptr, len) => {
    VG.enc ??= VG.device.createCommandEncoder();
    VG.pass = VG.enc.beginRenderPass(VG.json(ptr, len));
  },
  vngpu_end_pass: () => {
    VG.pass.end();
    VG.pass = null;
  },
  vngpu_set_pipeline: (p) => VG.pass.setPipeline(VG.objs[p]),
  vngpu_set_bind_group: (i, g, ptr, n) => {
    if (n) VG.pass.setBindGroup(i, VG.objs[g], new Uint32Array(wasmMemory.buffer, ptr >>> 0, n));
    else VG.pass.setBindGroup(i, VG.objs[g]);
  },
  vngpu_set_vertex_buffer: (slot, buf, off) => VG.pass.setVertexBuffer(slot, VG.objs[buf], off),
  vngpu_set_index_buffer: (buf, u32, off) => VG.pass.setIndexBuffer(VG.objs[buf], u32 ? 'uint32' : 'uint16', off),
  vngpu_set_viewport: (x, y, w, h, z0, z1) => VG.pass.setViewport(x, y, w, h, z0, z1),
  vngpu_set_scissor: (x, y, w, h) => VG.pass.setScissorRect(x, y, w, h),
  vngpu_set_blend_constant: (r, g, b, a) => VG.pass.setBlendConstant({ r, g, b, a }),
  vngpu_set_stencil_ref: (v) => VG.pass.setStencilReference(v),
  vngpu_draw: (n, inst, first, firstInst) => VG.pass.draw(n, inst, first, firstInst),
  vngpu_draw_indexed: (n, inst, first, base, firstInst) => VG.pass.drawIndexed(n, inst, first, base, firstInst),
});
