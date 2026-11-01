// Runs the WebGPU commands of Direct3D 9 games (crates/vnhost/src/d3d9): every frame, the batches
// Wine's threads closed are taken with _vnhost_d3d9_poll and executed on the page's device.
// The command words are listed in crates/vnhost/src/d3d9/gpu.rs.

export interface D3DWine {
  _malloc(size: number): number;
  _vnhost_d3d9_poll(out: number): number;
  _vnhost_d3d9_readback_done(id: number): void;
  wasmMemory: WebAssembly.Memory;
}

const CREATE_TEXTURE = 1,
  DESTROY_TEXTURE = 2,
  WRITE_TEXTURE = 3,
  CREATE_SHADER = 4,
  CREATE_PIPELINE = 5,
  SET_TARGET = 6,
  CLEAR = 7,
  DRAW = 8,
  PRESENT = 9,
  COPY = 10,
  READBACK = 11,
  FILL = 12;

const TEX_FORMATS: GPUTextureFormat[] = ["bgra8unorm", "depth24plus-stencil8", "rgba16float", "rgba16float"];
const VERTEX_FORMATS: GPUVertexFormat[] = [
  "float32", "float32x2", "float32x3", "float32x4", "unorm8x4", "uint8x4", "sint16x2", "sint16x4",
  "snorm16x2", "snorm16x4", "unorm16x2", "unorm16x4", "float16x2", "float16x4",
];
const TOPOLOGY: GPUPrimitiveTopology[] = ["point-list", "line-list", "line-strip", "triangle-list", "triangle-strip"];
const COMPARE: GPUCompareFunction[] = ["always", "never", "less", "equal", "less-equal", "greater", "not-equal", "greater-equal", "always"];
const BLEND: GPUBlendFactor[] = [
  "one", "zero", "one", "src", "one-minus-src", "src-alpha", "one-minus-src-alpha", "dst-alpha", "one-minus-dst-alpha",
  "dst", "one-minus-dst", "src-alpha-saturated", "src-alpha", "one-minus-src-alpha", "constant", "one-minus-constant",
];
const BLEND_ALPHA: GPUBlendFactor[] = [
  "one", "zero", "one", "src-alpha", "one-minus-src-alpha", "src-alpha", "one-minus-src-alpha", "dst-alpha",
  "one-minus-dst-alpha", "dst-alpha", "one-minus-dst-alpha", "one", "src-alpha", "one-minus-src-alpha", "constant",
  "one-minus-constant",
];
const BLEND_OP: GPUBlendOperation[] = ["add", "add", "subtract", "reverse-subtract", "min", "max"];
const ADDRESS: GPUAddressMode[] = ["repeat", "repeat", "mirror-repeat", "clamp-to-edge", "clamp-to-edge", "mirror-repeat"];

const CLEAR_SHADER = /* wgsl */ `
struct C { color: vec4f, z: vec4f };
@group(0) @binding(0) var<uniform> c: C;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
  let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
  return vec4f(p * 2.0 - 1.0, c.z.x, 1.0);
}
@fragment fn fs() -> @location(0) vec4f { return c.color; }
`;

const BLIT_SHADER = /* wgsl */ `
struct Out { @builtin(position) pos: vec4f, @location(0) uv: vec2f };
struct R { src: vec4f };
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
@group(0) @binding(2) var<uniform> r: R;
@vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
  let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
  var o: Out;
  o.pos = vec4f(p * 2.0 - 1.0, 0.0, 1.0);
  o.uv = r.src.xy + vec2f(p.x, 1.0 - p.y) * r.src.zw;
  return o;
}
@fragment fn fs(i: Out) -> @location(0) vec4f { return textureSample(tex, smp, i.uv); }
`;

interface Tex {
  tex: GPUTexture;
  w: number;
  h: number;
  levels: number;
  format: GPUTextureFormat;
  view: GPUTextureView;
}

interface Pipe {
  pipeline: GPURenderPipeline;
  depth: boolean;
}

/** What the page shows of a Direct3D window. */
export interface Overlay {
  view: GPUTextureView;
  /** client area on the screen: left, top, right, bottom */
  rect: [number, number, number, number];
}

export class D3D9 {
  private textures = new Map<number, Tex>();
  private shaders = new Map<number, GPUShaderModule>();
  private pipelines = new Map<number, Pipe>();
  private samplers = new Map<number, GPUSampler>();
  private groups1 = new Map<string, GPUBindGroup>();
  private groups0 = new Map<string, GPUBindGroup>();
  private layout0: GPUBindGroupLayout;
  private layout1: GPUBindGroupLayout;
  private layout: GPUPipelineLayout;
  private dataBuf: GPUBuffer | null = null;
  private white: GPUTextureView;
  private clearPipes = new Map<string, GPURenderPipeline>();
  private clearUniform: GPUBuffer;
  private blitPipes = new Map<string, GPURenderPipeline>();
  private staging = new Uint8Array(0);
  private io = 0;
  private wordsLeft = 0;
  overlay: Overlay | null = null;
  /** called after a Present */
  onPresent: (() => void) | null = null;
  frames = 0;
  private inflight = 0;
  draws = 0;

  // per batch
  private encoder: GPUCommandEncoder | null = null;
  private pass: GPURenderPassEncoder | null = null;
  private passDraws = 0;
  private target: [number, number, number] = [0, 0, 0];
  private pendingClear: { flags: number; color: GPUColor; z: number; stencil: number } | null = null;
  private used = new Set<number>();

  constructor(
    private readonly device: GPUDevice,
    private readonly wine: D3DWine,
  ) {
    const d = device;
    this.layout0 = d.createBindGroupLayout({
      entries: [0, 1].map((binding) => ({
        binding,
        visibility: GPUShaderStage.VERTEX | GPUShaderStage.FRAGMENT,
        buffer: { type: "uniform" as const, hasDynamicOffset: true },
      })),
    });
    const e1: GPUBindGroupLayoutEntry[] = [];
    for (let s = 0; s < 8; s++) {
      e1.push({ binding: s * 2, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: "float" } });
      e1.push({ binding: s * 2 + 1, visibility: GPUShaderStage.FRAGMENT, sampler: { type: "filtering" } });
    }
    this.layout1 = d.createBindGroupLayout({ entries: e1 });
    this.layout = d.createPipelineLayout({ bindGroupLayouts: [this.layout0, this.layout1] });
    const white = d.createTexture({ size: [1, 1], format: "bgra8unorm", usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST });
    d.queue.writeTexture({ texture: white }, new Uint8Array([255, 255, 255, 255]), { bytesPerRow: 4 }, [1, 1]);
    this.white = white.createView();
    this.clearUniform = d.createBuffer({ size: 256 * 64, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
  }

  /** Runs what Wine sent since the last call. */
  poll(): void {
    if (!this.io) this.io = this.wine._malloc(32) >>> 0;
    for (let n = 0; n < 64; n++) {
      // (the GPU at most two frames behind: the game then waits in Present)
      if (this.inflight >= 2) break;
      if (!this.wine._vnhost_d3d9_poll(this.io)) break;
      const heap = this.wine.wasmMemory.buffer;
      const u32 = new Uint32Array(heap, this.io, 6);
      const [cmds, ncmd, data, ndata, blob, nblob] = Array.from(u32, (x) => x >>> 0);
      const t0 = performance.now();
      try {
        this.run(new Uint32Array(heap, cmds!, ncmd!), new Uint8Array(heap, data!, ndata!), new Uint8Array(heap, blob!, nblob!));
      } catch (e) {
        console.error("d3d9:", e);
        this.endPass();
        this.submit();
      }
      this.runMs += performance.now() - t0;
    }
  }

  /** time spent running batches (ms, for stats) */
  runMs = 0;
  /** counts for stats (reset by the reader) */
  stat = { passes: 0, submits: 0, created: 0, uploaded: 0, gpuMs: 0, gpuMax: 0 };

  private copy(src: Uint8Array): Uint8Array<ArrayBuffer> {
    if (this.staging.length < src.length) this.staging = new Uint8Array(Math.max(src.length, this.staging.length * 2));
    this.staging.set(src);
    return this.staging.subarray(0, src.length);
  }

  private run(c: Uint32Array, data: Uint8Array, blob: Uint8Array): void {
    const d = this.device;
    if (data.length) {
      const size = Math.ceil(data.length / 4) * 4 + 65536;
      if (!this.dataBuf || this.dataBuf.size < size) {
        this.dataBuf?.destroy();
        this.dataBuf = d.createBuffer({
          size: Math.max(size, (this.dataBuf?.size ?? 0) * 2, 1 << 20),
          usage: GPUBufferUsage.VERTEX | GPUBufferUsage.INDEX | GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
        });
        this.groups0.clear();
      }
      const n = Math.ceil(data.length / 4) * 4;
      const src = this.copy(data);
      if (n !== src.length) {
        const padded = new Uint8Array(n);
        padded.set(src);
        d.queue.writeBuffer(this.dataBuf, 0, padded);
      } else d.queue.writeBuffer(this.dataBuf, 0, src);
    }
    this.target = [0, 0, 0];
    this.pendingClear = null;
    let i = 0;
    while (i < c.length) {
      const op = c[i]!;
      switch (op) {
        case CREATE_TEXTURE: {
          const [id, w, h, levels, format, rt] = [c[i + 1]!, c[i + 2]!, c[i + 3]!, c[i + 4]!, c[i + 5]!, c[i + 6]!];
          const fmt = TEX_FORMATS[format] ?? "bgra8unorm";
          const depth = fmt === "depth24plus-stencil8";
          const tex = d.createTexture({
            size: [w, h],
            format: fmt,
            mipLevelCount: depth ? 1 : levels,
            usage:
              GPUTextureUsage.TEXTURE_BINDING |
              GPUTextureUsage.COPY_DST |
              GPUTextureUsage.COPY_SRC |
              (rt || depth ? GPUTextureUsage.RENDER_ATTACHMENT : 0) |
              // StretchRect/ColorFill can draw into any color texture
              (depth ? 0 : GPUTextureUsage.RENDER_ATTACHMENT),
          });
          this.textures.get(id)?.tex.destroy();
          this.stat.created++;
          this.textures.set(id, { tex, w, h, levels: depth ? 1 : levels, format: fmt, view: tex.createView() });
          i += 7;
          break;
        }
        case DESTROY_TEXTURE: {
          const t = this.textures.get(c[i + 1]!);
          if (t) {
            this.flushIfUsed(c[i + 1]!);
            t.tex.destroy();
            this.textures.delete(c[i + 1]!);
            this.groups1.clear();
          }
          i += 2;
          break;
        }
        case WRITE_TEXTURE: {
          const [id, level, x, y, w, h, bpr, off] = Array.from(c.subarray(i + 1, i + 9));
          const t = this.textures.get(id!);
          if (t && t.format !== "depth24plus-stencil8") {
            this.flushIfUsed(id!);
            const bytes = blob.subarray(off!, off! + bpr! * h!);
            this.stat.uploaded += bytes.length;
            const lw = Math.max(1, t.w >> level!),
              lh = Math.max(1, t.h >> level!);
            const cw = Math.min(w!, lw - x!),
              ch = Math.min(h!, lh - y!);
            if (cw > 0 && ch > 0 && t.format === "bgra8unorm") {
              d.queue.writeTexture({ texture: t.tex, mipLevel: level!, origin: { x: x!, y: y! } }, this.copy(bytes), { bytesPerRow: bpr! }, [cw, ch]);
            } else if (cw > 0 && ch > 0 && bpr! === w! * 8) {
              d.queue.writeTexture({ texture: t.tex, mipLevel: level!, origin: { x: x!, y: y! } }, this.copy(bytes), { bytesPerRow: bpr! }, [cw, ch]);
            }
          }
          i += 9;
          break;
        }
        case CREATE_SHADER: {
          const [id, off, len] = [c[i + 1]!, c[i + 2]!, c[i + 3]!];
          const code = new TextDecoder().decode(blob.slice(off, off + len));
          const module = d.createShaderModule({ code });
          void module.getCompilationInfo().then((info) => {
            const errors = info.messages.filter((m) => m.type === "error");
            if (errors.length) console.error(`d3d9: shader ${id}: ${errors.map((m) => `${m.lineNum}: ${m.message}`).join("; ")}\n${code}`);
          });
          this.shaders.set(id, module);
          i += 4;
          break;
        }
        case CREATE_PIPELINE: {
          const id = c[i + 1]!,
            n = c[i + 2]!;
          this.pipelines.set(id, this.createPipeline(c.subarray(i + 3, i + 3 + n)));
          i += 3 + n;
          break;
        }
        case SET_TARGET: {
          const t: [number, number, number] = [c[i + 1]!, c[i + 2]!, c[i + 3]!];
          if (t[0] !== this.target[0] || t[1] !== this.target[1] || t[2] !== this.target[2]) {
            this.endPass();
            this.target = t;
            this.pendingClear = null;
          }
          i += 4;
          break;
        }
        case CLEAR: {
          const f32 = new Float32Array(c.buffer, c.byteOffset + (i + 2) * 4, 5);
          const flags = c[i + 1]!;
          const color = { r: f32[0]!, g: f32[1]!, b: f32[2]!, a: f32[3]! };
          const z = f32[4]!;
          const stencil = c[i + 7]!;
          const [x, y, w, h] = [c[i + 8]!, c[i + 9]!, c[i + 10]!, c[i + 11]!];
          if (w === 0) {
            // whole target: start a new pass that clears
            this.endPass();
            this.pendingClear = { flags, color, z, stencil };
          } else {
            this.clearRect(flags, color, z, x, y, w, h);
          }
          i += 12;
          break;
        }
        case DRAW:
          i = this.draw(c, i);
          break;
        case PRESENT: {
          const [tex, l, t, r, b] = [c[i + 1]!, c[i + 2]! | 0, c[i + 3]! | 0, c[i + 4]! | 0, c[i + 5]! | 0];
          this.endPass();
          this.submit();
          const tx = this.textures.get(tex);
          if (tx) this.overlay = { view: tx.view, rect: [l, t, r, b] };
          this.frames++;
          this.inflight++;
          const sent = performance.now();
          void this.device.queue.onSubmittedWorkDone().then(() => {
            this.inflight--;
            // how long the GPU took to finish the frame (queue wait included)
            const ms = performance.now() - sent;
            this.stat.gpuMs += ms;
            this.stat.gpuMax = Math.max(this.stat.gpuMax, ms);
          });
          this.onPresent?.();
          i += 10;
          break;
        }
        case COPY: {
          const a = Array.from(c.subarray(i + 1, i + 14));
          this.copyRect(a);
          i += 14;
          break;
        }
        case READBACK: {
          const [req, tex, level, x, y, w, h, dst, pitch] = Array.from(c.subarray(i + 1, i + 10));
          this.readback(req!, tex!, level!, x!, y!, w!, h!, dst!, pitch!);
          i += 10;
          break;
        }
        case FILL: {
          const [tex, level, x, y, w, h, color] = Array.from(c.subarray(i + 1, i + 8));
          const saved = this.target;
          this.endPass();
          this.target = [tex!, level!, 0];
          const col = { r: ((color! >> 16) & 255) / 255, g: ((color! >> 8) & 255) / 255, b: (color! & 255) / 255, a: (color! >>> 24) / 255 };
          this.clearRect(1, col, 0, x!, y!, w!, h!);
          this.endPass();
          this.target = saved;
          i += 8;
          break;
        }
        default:
          throw new Error(`unknown command ${op} at ${i}`);
      }
    }
    this.endPass();
    this.submit();
    void this.wordsLeft;
  }

  private enc(): GPUCommandEncoder {
    return (this.encoder ??= this.device.createCommandEncoder());
  }

  private submit(): void {
    if (this.encoder) {
      this.stat.submits++;
      this.device.queue.submit([this.encoder.finish()]);
      this.encoder = null;
      this.used.clear();
    }
  }

  /** Before writing a texture the open commands use: submits them first (queue order). */
  private flushIfUsed(id: number): void {
    if (this.used.has(id)) {
      const reopen = this.pass !== null;
      this.endPass();
      this.submit();
      void reopen;
    }
  }

  private endPass(): void {
    if (this.pass) {
      this.pass.end();
      this.pass = null;
    }
    // a whole-target clear nobody drew after still has to happen
    if (this.pendingClear && this.target[0]) {
      this.beginPass();
      this.pass!.end();
      this.pass = null;
    }
  }

  private beginPass(): GPURenderPassEncoder | null {
    if (this.pass) return this.pass;
    const t = this.textures.get(this.target[0]);
    if (!t) return null;
    const z = this.target[2] ? this.textures.get(this.target[2]) : undefined;
    const clear = this.pendingClear;
    this.pendingClear = null;
    this.used.add(this.target[0]);
    if (z) this.used.add(this.target[2]);
    const level = Math.min(this.target[1], t.levels - 1);
    this.pass = this.enc().beginRenderPass({
      colorAttachments: [
        {
          view: t.tex.createView({ baseMipLevel: level, mipLevelCount: 1 }),
          loadOp: clear && clear.flags & 1 ? "clear" : "load",
          clearValue: clear?.color,
          storeOp: "store",
        },
      ],
      depthStencilAttachment: z
        ? {
            view: z.view,
            depthLoadOp: clear && clear.flags & 2 ? "clear" : "load",
            depthClearValue: clear?.z ?? 1,
            depthStoreOp: "store",
            stencilLoadOp: clear && clear.flags & 4 ? "clear" : "load",
            stencilClearValue: clear?.stencil ?? 0,
            stencilStoreOp: "store",
          }
        : undefined,
    });
    this.passDraws = 0;
    this.stat.passes++;
    return this.pass;
  }

  private targetSize(): [number, number] {
    const t = this.textures.get(this.target[0]);
    if (!t) return [0, 0];
    return [Math.max(1, t.w >> this.target[1]), Math.max(1, t.h >> this.target[1])];
  }

  private clearRect(flags: number, color: GPUColor, z: number, x: number, y: number, w: number, h: number): void {
    const [tw, th] = this.targetSize();
    const x0 = Math.min(x, tw),
      y0 = Math.min(y, th);
    const x1 = Math.min(x + w, tw),
      y1 = Math.min(y + h, th);
    if (x1 <= x0 || y1 <= y0) return;
    if (x0 === 0 && y0 === 0 && x1 === tw && y1 === th) {
      this.endPass();
      this.pendingClear = { flags, color, z, stencil: 0 };
      return;
    }
    const pass = this.beginPass();
    if (!pass) return;
    const t = this.textures.get(this.target[0])!;
    const depth = this.target[2] !== 0;
    const key = `${t.format}/${depth}/${flags & 3}`;
    let pipe = this.clearPipes.get(key);
    if (!pipe) {
      const module = this.device.createShaderModule({ code: CLEAR_SHADER });
      pipe = this.device.createRenderPipeline({
        layout: "auto",
        vertex: { module, entryPoint: "vs" },
        fragment: { module, entryPoint: "fs", targets: [{ format: t.format, writeMask: flags & 1 ? 0xf : 0 }] },
        primitive: { topology: "triangle-list" },
        depthStencil: depth
          ? { format: "depth24plus-stencil8", depthWriteEnabled: (flags & 2) !== 0, depthCompare: "always" }
          : undefined,
      });
      this.clearPipes.set(key, pipe);
    }
    const c = color as GPUColorDict;
    const slot = (this.clearSlot = (this.clearSlot + 1) % 64);
    this.device.queue.writeBuffer(this.clearUniform, slot * 256, new Float32Array([c.r, c.g, c.b, c.a, z, 0, 0, 0]));
    const group = this.device.createBindGroup({
      layout: pipe.getBindGroupLayout(0),
      entries: [{ binding: 0, resource: { buffer: this.clearUniform, offset: slot * 256, size: 32 } }],
    });
    pass.setPipeline(pipe);
    pass.setBindGroup(0, group);
    pass.setViewport(0, 0, tw, th, 0, 1);
    pass.setScissorRect(x0, y0, x1 - x0, y1 - y0);
    pass.draw(3);
    this.passDraws++;
    // (the clear uniform slots are written now, at queue time: no more than 64 per batch)
    if (slot === 63) {
      this.endPass();
      this.submit();
    }
  }
  private clearSlot = 0;

  private createPipeline(k: Uint32Array): Pipe {
    const [vs, ps, topo, strip, cull, colorFmt, depthFmt, depthWrite, depthCmp] = Array.from(k.subarray(0, 9));
    const blend = Array.from(k.subarray(9, 16));
    const wmask = k[16]!;
    let p = 17;
    const nbuf = k[p++]!;
    const buffers: GPUVertexBufferLayout[] = [];
    for (let b = 0; b < nbuf; b++) {
      const stride = k[p++]!;
      const n = k[p++]!;
      const attributes: GPUVertexAttribute[] = [];
      for (let a = 0; a < n; a++) {
        attributes.push({ format: VERTEX_FORMATS[k[p]!]!, offset: k[p + 1]!, shaderLocation: k[p + 2]! });
        p += 3;
      }
      buffers.push({ arrayStride: Math.ceil(stride / 4) * 4, attributes });
    }
    const format = TEX_FORMATS[colorFmt!] ?? "bgra8unorm";
    const depth = depthFmt !== 0xff;
    const blendState: GPUBlendState | undefined = blend[0]
      ? {
          color: { srcFactor: BLEND[blend[1]!] ?? "one", dstFactor: BLEND[blend[2]!] ?? "zero", operation: BLEND_OP[blend[3]!] ?? "add" },
          alpha: {
            srcFactor: BLEND_ALPHA[blend[4]!] ?? "one",
            dstFactor: BLEND_ALPHA[blend[5]!] ?? "zero",
            operation: BLEND_OP[blend[6]!] ?? "add",
          },
        }
      : undefined;
    // min/max take factors one
    if (blendState) {
      for (const c of [blendState.color, blendState.alpha]) {
        if (c.operation === "min" || c.operation === "max") {
          c.srcFactor = "one";
          c.dstFactor = "one";
        }
      }
    }
    const pipeline = this.device.createRenderPipeline({
      layout: this.layout,
      vertex: { module: this.shaders.get(vs!)!, entryPoint: "main", buffers },
      fragment: { module: this.shaders.get(ps!)!, entryPoint: "main", targets: [{ format, blend: blendState, writeMask: wmask }] },
      primitive: {
        topology: TOPOLOGY[topo!] ?? "triangle-list",
        stripIndexFormat: strip === 2 ? "uint16" : strip === 4 ? "uint32" : undefined,
        cullMode: cull === 1 ? "front" : cull === 2 ? "back" : "none",
        frontFace: "ccw",
      },
      depthStencil: depth
        ? {
            format: "depth24plus-stencil8",
            depthWriteEnabled: depthWrite !== 0,
            depthCompare: COMPARE[depthCmp!] ?? "always",
          }
        : undefined,
    });
    return { pipeline, depth };
  }

  private sampler(key: number): GPUSampler {
    let s = this.samplers.get(key);
    if (!s) {
      const filter = (f: number): GPUFilterMode => (f >= 2 ? "linear" : "nearest");
      const mip = (key >> 10) & 3;
      s = this.device.createSampler({
        addressModeU: ADDRESS[key & 7] ?? "repeat",
        addressModeV: ADDRESS[(key >> 3) & 7] ?? "repeat",
        magFilter: filter((key >> 6) & 3),
        minFilter: filter((key >> 8) & 3),
        mipmapFilter: mip >= 2 ? "linear" : "nearest",
        lodMaxClamp: mip === 0 ? 0 : 32,
      });
      this.samplers.set(key, s);
    }
    return s;
  }

  private draw(c: Uint32Array, i: number): number {
    const pipe = this.pipelines.get(c[i + 1]!);
    const f32 = new Float32Array(c.buffer, c.byteOffset + (i + 2) * 4, 6);
    const [vx, vy, vw, vh, minz, maxz] = Array.from(f32);
    const [sx, sy, sw, sh] = [c[i + 8]!, c[i + 9]!, c[i + 10]!, c[i + 11]!];
    const [vso, vss, pso, pss] = [c[i + 12]!, c[i + 13]!, c[i + 14]!, c[i + 15]!];
    const blendFactor = c[i + 16]!;
    const ntex = c[i + 17]!;
    let p = i + 18;
    const texs: [number, number, number][] = [];
    for (let t = 0; t < ntex; t++) {
      texs.push([c[p]!, c[p + 1]!, c[p + 2]!]);
      p += 3;
    }
    const nbuf = c[p++]!;
    const offs = Array.from(c.subarray(p, p + nbuf));
    p += nbuf;
    const [ifmt, ioff, count, base] = [c[p]!, c[p + 1]!, c[p + 2]!, c[p + 3]! | 0];
    p += 4;
    if (!pipe || !this.dataBuf || count === 0) return p;
    const [tw, th] = this.targetSize();
    // viewport and scissor inside the target
    const x0 = Math.max(0, Math.min(sx, tw)),
      y0 = Math.max(0, Math.min(sy, th));
    const x1 = Math.max(x0, Math.min(sx + sw, tw)),
      y1 = Math.max(y0, Math.min(sy + sh, th));
    if (x1 <= x0 || y1 <= y0) return p;
    const vpx = Math.max(-32768, vx!),
      vpy = Math.max(-32768, vy!);
    const vpw = Math.max(1, Math.min(vw!, 32768)),
      vph = Math.max(1, Math.min(vh!, 32768));
    const pass = this.beginPass();
    if (!pass) return p;
    for (const [, tex] of texs) if (tex) this.used.add(tex);
    pass.setPipeline(pipe.pipeline);
    // WebGPU wants the viewport inside the target
    const cx0 = Math.max(0, vpx),
      cy0 = Math.max(0, vpy);
    const cx1 = Math.min(tw, vpx + vpw),
      cy1 = Math.min(th, vpy + vph);
    if (cx1 <= cx0 || cy1 <= cy0) return p;
    pass.setViewport(vpx === cx0 ? vpx : cx0, vpy === cy0 ? vpy : cy0, cx1 - cx0, cy1 - cy0, Math.min(minz!, maxz!), Math.max(minz!, maxz!));
    pass.setScissorRect(Math.max(x0, Math.floor(cx0)), Math.max(y0, Math.floor(cy0)), 0, 0);
    const sx0 = Math.max(x0, Math.floor(cx0)),
      sy0 = Math.max(y0, Math.floor(cy0));
    const sx1 = Math.min(x1, Math.ceil(cx1)),
      sy1 = Math.min(y1, Math.ceil(cy1));
    if (sx1 <= sx0 || sy1 <= sy0) return p;
    pass.setScissorRect(sx0, sy0, sx1 - sx0, sy1 - sy0);
    const k0 = `${vss}/${pss}`;
    let g0 = this.groups0.get(k0);
    if (!g0) {
      g0 = this.device.createBindGroup({
        layout: this.layout0,
        entries: [
          { binding: 0, resource: { buffer: this.dataBuf, offset: 0, size: vss } },
          { binding: 1, resource: { buffer: this.dataBuf, offset: 0, size: pss } },
        ],
      });
      this.groups0.set(k0, g0);
    }
    pass.setBindGroup(0, g0, [vso, pso]);
    const k1 = texs.map((t) => t.join(",")).join(";");
    let g1 = this.groups1.get(k1);
    if (!g1) {
      const entries: GPUBindGroupEntry[] = [];
      for (let s = 0; s < 8; s++) {
        const t = texs.find((x) => x[0] === s);
        const tex = t ? this.textures.get(t[1]) : undefined;
        const ok = tex && tex.format !== "depth24plus-stencil8";
        entries.push({ binding: s * 2, resource: ok ? tex.view : this.white });
        entries.push({ binding: s * 2 + 1, resource: this.sampler(t ? t[2] : 0x5) });
      }
      g1 = this.device.createBindGroup({ layout: this.layout1, entries });
      if (this.groups1.size > 4096) this.groups1.clear();
      this.groups1.set(k1, g1);
    }
    pass.setBindGroup(1, g1);
    offs.forEach((o, b) => pass.setVertexBuffer(b, this.dataBuf!, o));
    pass.setBlendConstant({
      r: ((blendFactor >> 16) & 255) / 255,
      g: ((blendFactor >> 8) & 255) / 255,
      b: (blendFactor & 255) / 255,
      a: (blendFactor >>> 24) / 255,
    });
    if (ifmt) {
      pass.setIndexBuffer(this.dataBuf, ifmt === 4 ? "uint32" : "uint16", ioff);
      pass.drawIndexed(count, 1, 0, base);
    } else {
      pass.draw(count);
    }
    this.passDraws++;
    this.draws++;
    return p;
  }

  private copyRect(a: number[]): void {
    const [st, sl, sx, sy, sw, sh, dt, dl, dx, dy, dw, dh, filter] = a as [number, number, number, number, number, number, number, number, number, number, number, number, number];
    const s = this.textures.get(st),
      t = this.textures.get(dt);
    if (!s || !t || sw <= 0 || sh <= 0 || dw <= 0 || dh <= 0) return;
    const saved = this.target;
    this.endPass();
    if (sw === dw && sh === dh && s.format === t.format && st !== dt) {
      this.enc().copyTextureToTexture(
        { texture: s.tex, mipLevel: sl, origin: { x: sx, y: sy } },
        { texture: t.tex, mipLevel: dl, origin: { x: dx, y: dy } },
        [sw, sh],
      );
      this.used.add(st);
      this.used.add(dt);
      return;
    }
    if (st === dt || t.format === "depth24plus-stencil8") return;
    let pipe = this.blitPipes.get(t.format);
    if (!pipe) {
      const module = this.device.createShaderModule({ code: BLIT_SHADER });
      pipe = this.device.createRenderPipeline({
        layout: "auto",
        vertex: { module, entryPoint: "vs" },
        fragment: { module, entryPoint: "fs", targets: [{ format: t.format }] },
        primitive: { topology: "triangle-list" },
      });
      this.blitPipes.set(t.format, pipe);
    }
    const slot = (this.clearSlot = (this.clearSlot + 1) % 64);
    const lw = Math.max(1, s.w >> sl),
      lh = Math.max(1, s.h >> sl);
    this.device.queue.writeBuffer(this.clearUniform, slot * 256, new Float32Array([sx / lw, sy / lh, sw / lw, sh / lh]));
    const group = this.device.createBindGroup({
      layout: pipe.getBindGroupLayout(0),
      entries: [
        { binding: 0, resource: s.tex.createView({ baseMipLevel: sl, mipLevelCount: 1 }) },
        { binding: 1, resource: this.sampler(filter ? 0x3 << 0 | 3 << 3 | 2 << 6 | 2 << 8 : 0x1b) },
        { binding: 2, resource: { buffer: this.clearUniform, offset: slot * 256, size: 16 } },
      ],
    });
    this.target = [dt, dl, 0];
    const pass = this.beginPass()!;
    pass.setPipeline(pipe);
    pass.setBindGroup(0, group);
    pass.setViewport(dx, dy, dw, dh, 0, 1);
    pass.draw(3);
    this.used.add(st);
    this.endPass();
    this.target = saved;
    if (slot === 63) this.submit();
  }

  private readback(req: number, id: number, level: number, x: number, y: number, w: number, h: number, dst: number, pitch: number): void {
    const t = this.textures.get(id);
    const done = () => this.wine._vnhost_d3d9_readback_done(req);
    if (!t || t.format !== "bgra8unorm") {
      done();
      return;
    }
    this.endPass();
    const bpr = Math.ceil((w * 4) / 256) * 256;
    const buf = this.device.createBuffer({ size: bpr * h, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    this.enc().copyTextureToBuffer({ texture: t.tex, mipLevel: level, origin: { x, y } }, { buffer: buf, bytesPerRow: bpr }, [w, h]);
    this.submit();
    void buf.mapAsync(GPUMapMode.READ).then(
      () => {
        const src = new Uint8Array(buf.getMappedRange());
        const heap = new Uint8Array(this.wine.wasmMemory.buffer);
        for (let r = 0; r < h; r++) heap.set(src.subarray(r * bpr, r * bpr + w * 4), dst + r * pitch);
        buf.unmap();
        buf.destroy();
        done();
      },
      () => done(),
    );
  }

  /** The last presented frame as BGRA rows (for tests). */
  async capture(): Promise<{ w: number; h: number; data: Uint8Array } | null> {
    const o = this.overlay;
    if (!o) return null;
    const t = [...this.textures.values()].find((x) => x.view === o.view);
    if (!t) return null;
    const bpr = Math.ceil((t.w * 4) / 256) * 256;
    const buf = this.device.createBuffer({ size: bpr * t.h, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    const enc = this.device.createCommandEncoder();
    enc.copyTextureToBuffer({ texture: t.tex }, { buffer: buf, bytesPerRow: bpr }, [t.w, t.h]);
    this.device.queue.submit([enc.finish()]);
    await buf.mapAsync(GPUMapMode.READ);
    const src = new Uint8Array(buf.getMappedRange());
    const data = new Uint8Array(t.w * t.h * 4);
    for (let r = 0; r < t.h; r++) data.set(src.subarray(r * bpr, r * bpr + t.w * 4), r * t.w * 4);
    buf.unmap();
    buf.destroy();
    return { w: t.w, h: t.h, data };
  }
}
