// Runs the WebGPU commands of Direct3D 9 games (crates/vnhost/src/d3d9): every frame, the batches
// Wine's threads closed are taken with _vnhost_d3d9_poll and executed on the page's device.
// The command words are listed in crates/vnhost/src/d3d9/gpu.rs.

export interface D3DWine {
  _malloc(size: number): number;
  _vnhost_d3d9_poll(out: number): number;
  _vnhost_d3d9_readback_done(id: number): void;
  _vnhost_d3d9_readback_begin(id: number): number;
  _vnhost_d3d9_done(): void;
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
  FILL = 12,
  WRITE_TEXTURE_MEM = 13,
  // Direct3D 11 (crates/vnhost/src/d3d11)
  CREATE_BUFFER = 20,
  DESTROY_BUFFER = 21,
  WRITE_BUFFER = 22,
  CREATE_PIPELINE2 = 23,
  SET_TARGETS2 = 24,
  DRAW2 = 25,
  CLEAR_VIEW = 26,
  GEN_MIPS = 27,
  CREATE_TEXTURE2 = 28,
  WRITE_TEXTURE2 = 29,
  PRESENT_FLIP = 30;
/** buffer id of the batch's data block (DRAW2 vertex/index/uniform sources) */
const DATA = 0xffffffff;

// (crates/vnhost/src/d3d11/format.rs P_*)
const TEX_FORMATS: GPUTextureFormat[] = [
  "bgra8unorm", "depth24plus-stencil8", "rgba16float", "rgba16float", "rgba8unorm", "r8unorm", "rg8unorm",
  "depth32float", "r16float", "rg16float", "rgb10a2unorm", "rg11b10ufloat", "rgba8unorm-srgb", "bgra8unorm-srgb",
  "depth16unorm", "r32float", "rg32float", "rgba8uint", "rgba8snorm", "rgba8sint", "r32uint", "r32sint",
];
const TEX_BPP: Record<string, number> = { r8unorm: 1, rg8unorm: 2, r16float: 2, depth16unorm: 2, rgba16float: 8, rg32float: 8 };
const isDepth = (f: GPUTextureFormat) => f.startsWith("depth");
// (crates/vnhost/src/d3d11/format.rs vertex_format)
const VERTEX_FORMATS2: GPUVertexFormat[] = [
  "float32", "float32x2", "float32x3", "float32x4", "uint32", "uint32x2", "uint32x3", "uint32x4",
  "sint32", "sint32x2", "sint32x3", "sint32x4", "unorm8x4", "snorm8x4", "uint8x4", "sint8x4",
  "unorm16x2", "unorm16x4", "snorm16x2", "snorm16x4", "uint16x2", "uint16x4", "sint16x2", "sint16x4",
  "float16x2", "float16x4", "unorm8x2", "uint8x2", "unorm10-10-10-2", "unorm8x4-bgra", "snorm8x2", "sint8x2",
];
const BLEND2: GPUBlendFactor[] = [
  "zero", "one", "src", "one-minus-src", "src-alpha", "one-minus-src-alpha", "dst-alpha", "one-minus-dst-alpha",
  "dst", "one-minus-dst", "src-alpha-saturated", "constant", "one-minus-constant",
];
const STENCIL_OP: GPUStencilOperation[] = [
  "keep", "keep", "zero", "replace", "increment-clamp", "decrement-clamp", "invert", "increment-wrap", "decrement-wrap",
];
const ADDRESS2: GPUAddressMode[] = ["repeat", "mirror-repeat", "clamp-to-edge", "clamp-to-edge"];
const VIEW_DIM: GPUTextureViewDimension[] = ["1d", "2d", "2d-array", "cube", "cube-array", "3d"];
const f32of = (u: number) => new Float32Array(new Uint32Array([u]).buffer)[0]!;
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
  /** Direct3D 11: array layers (depth of 3D textures), dimension, views made */
  layers?: number;
  dim?: GPUTextureDimension;
  views?: Map<string, GPUTextureView>;
}

/** Direct3D 11 pipeline: made at its first draw (the bind group layouts come from the draw) */
interface Pipe2 {
  key: Uint32Array;
  pipeline?: GPURenderPipeline;
  layouts?: GPUBindGroupLayout[];
  /** entries kinds of each group, to find a draw that binds differently */
  kinds?: string;
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
  /** bottom row first (OpenGL) */
  flip?: boolean;
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
  // Direct3D 11
  private buffers = new Map<number, GPUBuffer>();
  private doomed: (GPUBuffer | GPUTexture)[] = [];
  private pipelines2 = new Map<number, Pipe2>();
  private targets2: number[] | null = null;
  private dummies = new Map<string, GPUTextureView>();
  private zeroBuf: GPUBuffer | null = null;
  private samplers2 = new Map<string, GPUSampler>();

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
      // (guest memory the batch read may change now)
      this.wine._vnhost_d3d9_done();
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
        case WRITE_TEXTURE_MEM: {
          // rows straight from guest memory (the guest leaves them alone until this batch ran)
          const [id, level, x, y, w, h, pitch, ptr] = Array.from(c.subarray(i + 1, i + 9), (v) => v >>> 0);
          const t = this.textures.get(id!);
          if (t && t.format !== "depth24plus-stencil8") {
            this.flushIfUsed(id!);
            const lw = Math.max(1, t.w >> level!),
              lh = Math.max(1, t.h >> level!);
            const cw = Math.min(w!, lw - x!),
              ch = Math.min(h!, lh - y!);
            const bpp = t.format === "bgra8unorm" ? 4 : 8;
            if (cw > 0 && ch > 0) {
              const bytes = new Uint8Array(this.wine.wasmMemory.buffer, ptr!, pitch! * (ch - 1) + cw * bpp);
              this.stat.uploaded += bytes.length;
              d.queue.writeTexture({ texture: t.tex, mipLevel: level!, origin: { x: x!, y: y! } }, this.copy(bytes), { bytesPerRow: pitch! }, [cw, ch]);
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
          if (this.targets2) {
            this.endPass();
            this.targets2 = null;
          }
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
        case PRESENT:
        case PRESENT_FLIP: {
          const [tex, l, t, r, b] = [c[i + 1]!, c[i + 2]! | 0, c[i + 3]! | 0, c[i + 4]! | 0, c[i + 5]! | 0];
          this.endPass();
          this.submit();
          const tx = this.textures.get(tex);
          if (tx) this.overlay = { view: tx.view, rect: [l, t, r, b], flip: op === PRESENT_FLIP };
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
          if (this.dump > 0 && --this.dump === 0) console.log("d3d9 dump done");
          else if (this.dump > 0) console.log("d3d9 ---- present");
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
        case CREATE_TEXTURE2:
          this.createTexture2(c.subarray(i + 1, i + 8));
          i += 8;
          break;
        case WRITE_TEXTURE2: {
          const [id, level, layer, x, y, w, h, bpr, off] = Array.from(c.subarray(i + 1, i + 10));
          const t = this.textures.get(id!);
          if (t && !isDepth(t.format)) {
            this.flushIfUsed(id!);
            const lw = Math.max(1, t.w >> level!),
              lh = Math.max(1, t.h >> level!);
            const cw = Math.min(w!, lw - x!),
              ch = Math.min(h!, lh - y!);
            if (cw > 0 && ch > 0 && level! < t.levels) {
              const bytes = blob.subarray(off!, off! + bpr! * h!);
              this.stat.uploaded += bytes.length;
              d.queue.writeTexture(
                { texture: t.tex, mipLevel: level!, origin: { x: x!, y: y!, z: layer! } },
                this.copy(bytes),
                { bytesPerRow: bpr!, rowsPerImage: h! },
                [cw, ch, 1],
              );
            }
          }
          i += 10;
          break;
        }
        case CREATE_BUFFER: {
          const [id, size] = [c[i + 1]!, c[i + 2]!];
          this.buffers.set(
            id,
            d.createBuffer({
              size: Math.ceil(size / 4) * 4,
              usage: GPUBufferUsage.VERTEX | GPUBufferUsage.INDEX | GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST | GPUBufferUsage.COPY_SRC,
            }),
          );
          i += 3;
          break;
        }
        case DESTROY_BUFFER: {
          const b = this.buffers.get(c[i + 1]!);
          if (b) {
            this.doomed.push(b);
            this.buffers.delete(c[i + 1]!);
          }
          i += 2;
          break;
        }
        case WRITE_BUFFER: {
          const [id, at, off, len] = Array.from(c.subarray(i + 1, i + 5));
          const b = this.buffers.get(id!);
          if (b) {
            const n = Math.min(Math.ceil(len! / 4) * 4, b.size - at!);
            const src = new Uint8Array(n);
            src.set(blob.subarray(off!, off! + Math.min(len!, n)));
            d.queue.writeBuffer(b, at!, src);
          }
          i += 5;
          break;
        }
        case CREATE_PIPELINE2: {
          const id = c[i + 1]!,
            n = c[i + 2]!;
          this.pipelines2.set(id, { key: c.slice(i + 3, i + 3 + n) });
          i += 3 + n;
          break;
        }
        case SET_TARGETS2: {
          const n = c[i + 1]!;
          const t = [n, ...Array.from(c.subarray(i + 2, i + 2 + n * 3 + 3))];
          const cur = this.targets2;
          if (!cur || cur.length !== t.length || cur.some((v, k) => v !== t[k])) {
            this.endPass();
            this.target = [0, 0, 0];
            this.pendingClear = null;
            this.targets2 = t;
          }
          i += 2 + n * 3 + 3;
          break;
        }
        case DRAW2:
          i = this.draw2(c, i);
          break;
        case CLEAR_VIEW: {
          const [tex, level, layer, flags] = Array.from(c.subarray(i + 1, i + 5));
          const f = new Float32Array(c.buffer, c.byteOffset + (i + 5) * 4, 5);
          this.clearView(tex!, level!, layer!, flags!, { r: f[0]!, g: f[1]!, b: f[2]!, a: f[3]! }, f[4]!, c[i + 10]!);
          i += 11;
          break;
        }
        case GEN_MIPS:
          this.genMips(c[i + 1]!);
          i += 2;
          break;
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
    // (after the commands that use them are submitted)
    for (const x of this.doomed) x.destroy();
    this.doomed.length = 0;
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

  /** frames whose passes go to the console (window.vnvmD3d.dump = n, for debugging) */
  dump = 0;
  private passInfo = "";

  private endPass(): void {
    if (this.pass) {
      if (this.dump > 0) console.log(`d3d9 pass ${this.passInfo} draws ${this.passDraws}`);
      this.pass.end();
      this.pass = null;
    }
    // a whole-target clear nobody drew after still has to happen
    if (this.pendingClear && this.target[0]) {
      // (no pass when the target is gone: the clear goes with it)
      this.beginPass()?.end();
      this.pendingClear = null;
      this.pass = null;
    }
  }

  private beginPass(): GPURenderPassEncoder | null {
    if (this.pass) return this.pass;
    if (this.targets2) return this.beginPass2(this.targets2);
    const t = this.textures.get(this.target[0]);
    if (!t) return null;
    const level = Math.min(this.target[1], t.levels - 1);
    const z = this.target[2] ? this.depthFor(this.textures.get(this.target[2]), Math.max(1, t.w >> level), Math.max(1, t.h >> level)) : undefined;
    const clear = this.pendingClear;
    this.pendingClear = null;
    this.used.add(this.target[0]);
    if (z) this.used.add(this.target[2]);
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
    if (this.dump > 0) {
      const [w, h] = this.targetSize();
      this.passInfo = `tex ${this.target[0]}.${this.target[1]} ${w}x${h} ${t.format}${z ? " +z" : ""} ${clear ? `clear ${clear.flags}` : "load"}`;
    }
    return this.pass;
  }

  /** Direct3D 9 lets a depth buffer be larger than the color target; WebGPU wants equal
   * sizes, so such passes get a depth texture of the target's size (its contents are not
   * shared with the real one). */
  private depthFor(z: Tex | undefined, w: number, h: number): Tex | undefined {
    if (!z || (z.w === w && z.h === h)) return z;
    const key = `${w}x${h}`;
    const zz = z as Tex & { fits?: Map<string, Tex> };
    zz.fits ??= new Map();
    let f = zz.fits.get(key);
    if (!f) {
      const tex = this.device.createTexture({ size: [w, h], format: z.format, usage: GPUTextureUsage.RENDER_ATTACHMENT });
      f = { tex, w, h, levels: 1, format: z.format, view: tex.createView() };
      zz.fits.set(key, f);
    }
    return f;
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
    // (sampling the texture being drawn to is not allowed in WebGPU: it reads as white)
    const rt = this.targets2 ? 0 : this.target[0];
    const k1 = texs.map((t) => t.join(",")).join(";") + (rt && texs.some((t) => t[1] === rt) ? `|${rt}` : "");
    let g1 = this.groups1.get(k1);
    if (!g1) {
      const entries: GPUBindGroupEntry[] = [];
      for (let s = 0; s < 8; s++) {
        const t = texs.find((x) => x[0] === s);
        const tex = t ? this.textures.get(t[1]) : undefined;
        const ok = tex && tex.format !== "depth24plus-stencil8" && !(rt && t![1] === rt);
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
    if (this.dump > 0) console.log(`d3d9 copy ${st}.${sl} ${sx},${sy} ${sw}x${sh} -> ${dt}.${dl} ${dx},${dy} ${dw}x${dh}`);
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
    if (!t || isDepth(t.format)) {
      done();
      return;
    }
    this.endPass();
    const bpp = TEX_BPP[t.format] ?? 4;
    const bpr = Math.ceil((w * bpp) / 256) * 256;
    const buf = this.device.createBuffer({ size: bpr * h, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    this.enc().copyTextureToBuffer({ texture: t.tex, mipLevel: level, origin: { x, y } }, { buffer: buf, bytesPerRow: bpr }, [w, h]);
    this.submit();
    void buf.mapAsync(GPUMapMode.READ).then(
      () => {
        // (not after the guest stopped waiting: it may have freed dst)
        if (this.wine._vnhost_d3d9_readback_begin(req)) {
          const src = new Uint8Array(buf.getMappedRange());
          const heap = new Uint8Array(this.wine.wasmMemory.buffer);
          for (let r = 0; r < h; r++) heap.set(src.subarray(r * bpr, r * bpr + w * bpp), dst + r * pitch);
        }
        buf.unmap();
        buf.destroy();
        done();
      },
      () => done(),
    );
  }

  // ---- Direct3D 11 ----

  private createTexture2(a: Uint32Array): void {
    const [id, w, h, layers, mips, format, dimCode] = Array.from(a) as [number, number, number, number, number, number, number];
    const fmt = TEX_FORMATS[format] ?? "bgra8unorm";
    const dim: GPUTextureDimension = dimCode === 1 ? "3d" : dimCode === 2 ? "1d" : "2d";
    const depth = isDepth(fmt);
    const renderable =
      dim === "2d" && !fmt.endsWith("snorm") && (fmt !== "rg11b10ufloat" || this.device.features.has("rg11b10ufloat-renderable"));
    const levels = dim === "1d" ? 1 : mips;
    const tex = this.device.createTexture({
      size: [w, dim === "1d" ? 1 : h, layers],
      dimension: dim,
      format: fmt,
      mipLevelCount: levels,
      usage:
        GPUTextureUsage.TEXTURE_BINDING |
        GPUTextureUsage.COPY_DST |
        GPUTextureUsage.COPY_SRC |
        (renderable || depth ? GPUTextureUsage.RENDER_ATTACHMENT : 0),
    });
    const old = this.textures.get(id);
    if (old) this.doomed.push(old.tex);
    this.stat.created++;
    const view = tex.createView({ dimension: dim === "2d" ? "2d" : dim, arrayLayerCount: dim === "2d" ? 1 : undefined, aspect: depth ? "depth-only" : "all" });
    this.textures.set(id, { tex, w, h, levels, format: fmt, view, layers, dim, views: new Map() });
  }

  /** A view of `t` for binding or rendering (cached on the texture). */
  private view2(t: Tex, dim: GPUTextureViewDimension, mip: number, mips: number, layer: number, layers: number, aspect: GPUTextureAspect = "all"): GPUTextureView {
    const k = `${dim}/${mip}/${mips}/${layer}/${layers}/${aspect}`;
    t.views ??= new Map();
    let v = t.views.get(k);
    if (!v) {
      const all = t.layers ?? 1;
      const base = Math.min(layer, all - 1);
      let count = dim === "2d" || dim === "1d" || dim === "3d" ? 1 : Math.max(1, Math.min(layers, all - base));
      if (dim === "cube") count = 6;
      if (dim === "cube-array") count = Math.max(6, Math.floor(count / 6) * 6);
      const m = Math.min(mip, t.levels - 1);
      v = t.tex.createView({
        dimension: dim,
        baseMipLevel: m,
        mipLevelCount: Math.max(1, Math.min(mips, t.levels - m)),
        baseArrayLayer: dim === "3d" ? 0 : base,
        arrayLayerCount: dim === "3d" ? 1 : count,
        aspect,
      });
      t.views.set(k, v);
    }
    return v;
  }

  private beginPass2(tg: number[]): GPURenderPassEncoder | null {
    const n = tg[0]!;
    const colors: GPURenderPassColorAttachment[] = [];
    for (let k = 0; k < n; k++) {
      const t = this.textures.get(tg[1 + k * 3]!);
      if (!t) return null;
      this.used.add(tg[1 + k * 3]!);
      colors.push({ view: this.view2(t, "2d", tg[2 + k * 3]!, 1, tg[3 + k * 3]!, 1), loadOp: "load", storeOp: "store" });
    }
    const zi = 1 + n * 3;
    const z = tg[zi] ? this.textures.get(tg[zi]!) : undefined;
    if (z) this.used.add(tg[zi]!);
    if (!colors.length && !z) return null;
    const stencil = z?.format === "depth24plus-stencil8";
    this.pass = this.enc().beginRenderPass({
      colorAttachments: colors,
      depthStencilAttachment: z
        ? {
            view: this.view2(z, "2d", tg[zi + 1]!, 1, tg[zi + 2]!, 1),
            depthLoadOp: "load",
            depthStoreOp: "store",
            ...(stencil ? { stencilLoadOp: "load" as const, stencilStoreOp: "store" as const } : {}),
          }
        : undefined,
    });
    this.passDraws = 0;
    this.stat.passes++;
    return this.pass;
  }

  /** The size of the Direct3D 11 targets' first level. */
  private targetSize2(): [number, number] {
    const tg = this.targets2;
    if (!tg) return [0, 0];
    const n = tg[0]!;
    const [id, level] = n ? [tg[1]!, tg[2]!] : [tg[1 + n * 3]!, tg[2 + n * 3]!];
    const t = this.textures.get(id);
    if (!t) return [0, 0];
    return [Math.max(1, t.w >> level), Math.max(1, t.h >> level)];
  }

  private clearView(tex: number, level: number, layer: number, flags: number, color: GPUColorDict, depth: number, stencil: number): void {
    const t = this.textures.get(tex);
    if (!t) return;
    this.endPass();
    this.used.add(tex);
    const view = this.view2(t, "2d", level, 1, layer, 1);
    if (isDepth(t.format)) {
      const s = t.format === "depth24plus-stencil8";
      this.enc()
        .beginRenderPass({
          colorAttachments: [],
          depthStencilAttachment: {
            view,
            depthLoadOp: flags & 2 ? "clear" : "load",
            depthClearValue: Math.min(1, Math.max(0, depth)),
            depthStoreOp: "store",
            ...(s ? { stencilLoadOp: flags & 4 ? ("clear" as const) : ("load" as const), stencilClearValue: stencil, stencilStoreOp: "store" as const } : {}),
          },
        })
        .end();
    } else if (flags & 1) {
      this.enc().beginRenderPass({ colorAttachments: [{ view, loadOp: "clear", clearValue: color, storeOp: "store" }] }).end();
    }
    this.stat.passes++;
  }

  private genMips(tex: number): void {
    const t = this.textures.get(tex);
    if (!t || t.levels < 2 || t.dim !== "2d" || isDepth(t.format)) return;
    this.endPass();
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
    this.device.queue.writeBuffer(this.clearUniform, slot * 256, new Float32Array([0, 0, 1, 1]));
    const smp = this.sampler2(1 << 9 | 1 << 10 | 2 | 2 << 3 | 2 << 6, 0, 0);
    for (let layer = 0; layer < (t.layers ?? 1); layer++) {
      for (let l = 1; l < t.levels; l++) {
        const group = this.device.createBindGroup({
          layout: pipe.getBindGroupLayout(0),
          entries: [
            { binding: 0, resource: this.view2(t, "2d", l - 1, 1, layer, 1) },
            { binding: 1, resource: smp },
            { binding: 2, resource: { buffer: this.clearUniform, offset: slot * 256, size: 16 } },
          ],
        });
        const pass = this.enc().beginRenderPass({
          colorAttachments: [{ view: this.view2(t, "2d", l, 1, layer, 1), loadOp: "clear", storeOp: "store" }],
        });
        pass.setPipeline(pipe);
        pass.setBindGroup(0, group);
        pass.draw(3);
        pass.end();
      }
    }
    this.used.add(tex);
    if (slot === 63) this.submit();
  }

  /** sampler key: crates/vnhost/src/d3d11 sampler_key; LOD clamps as f32 bits */
  private sampler2(key: number, minLod: number, maxLod: number): GPUSampler {
    const k = `${key}/${minLod}/${maxLod}`;
    let s = this.samplers2.get(k);
    if (!s) {
      const aniso = (key >> 12) & 1;
      const lin = (b: number): GPUFilterMode => (aniso || (key >> b) & 1 ? "linear" : "nearest");
      const func = (key >> 13) & 15;
      const lo = Math.min(32, Math.max(0, f32of(minLod) || 0));
      const hi = Math.min(32, Math.max(lo, f32of(maxLod) || 0));
      s = this.device.createSampler({
        addressModeU: ADDRESS2[key & 7] ?? "repeat",
        addressModeV: ADDRESS2[(key >> 3) & 7] ?? "repeat",
        addressModeW: ADDRESS2[(key >> 6) & 7] ?? "repeat",
        magFilter: lin(9),
        minFilter: lin(10),
        mipmapFilter: aniso || (key >> 11) & 1 ? "linear" : "nearest",
        lodMinClamp: lo,
        lodMaxClamp: hi,
        maxAnisotropy: aniso ? Math.max(1, Math.min(16, (key >> 17) & 31)) : 1,
        compare: func ? COMPARE[func] : undefined,
      });
      this.samplers2.set(k, s);
    }
    return s;
  }

  /** A 1x1 texture of zeros for unbound slots: view dimension, kind (1 float, 4 uint, 5 sint, 6 depth). */
  private dummy(vdim: number, kind: number): GPUTextureView {
    const k = `${vdim}/${kind}`;
    let v = this.dummies.get(k);
    if (!v) {
      const dim = VIEW_DIM[vdim] ?? "2d";
      const format: GPUTextureFormat = kind === 4 ? "rgba8uint" : kind === 5 ? "rgba8sint" : kind === 6 ? "depth32float" : "rgba8unorm";
      const layers = dim === "cube" || dim === "cube-array" ? 6 : 1;
      const tex = this.device.createTexture({
        size: [1, 1, layers],
        dimension: dim === "1d" ? "1d" : dim === "3d" ? "3d" : "2d",
        format,
        usage: GPUTextureUsage.TEXTURE_BINDING | (kind === 6 ? GPUTextureUsage.RENDER_ATTACHMENT : GPUTextureUsage.COPY_DST),
      });
      v = tex.createView({ dimension: dim });
      this.dummies.set(k, v);
    }
    return v;
  }

  private zero(): GPUBuffer {
    return (this.zeroBuf ??= this.device.createBuffer({ size: 65536, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.VERTEX | GPUBufferUsage.INDEX }));
  }

  /** The pipeline of `p` with bind group layouts for `groups` (entries of 8 words). */
  private pipeline2(p: Pipe2, groups: Uint32Array[]): boolean {
    const kinds = groups.map((g) => Array.from({ length: g.length / 8 }, (_, e) => `${g[e * 8]}:${g[e * 8 + 1]}:${g[e * 8 + 1] === 1 || g[e * 8 + 1]! >= 4 ? g[e * 8 + 3] : ""}`).join(",")).join("|");
    if (p.pipeline && p.kinds === kinds) return true;
    const k = p.key;
    const layouts = groups.map((g, stage) => {
      const entries: GPUBindGroupLayoutEntry[] = [];
      const visibility = stage === 0 ? GPUShaderStage.VERTEX : GPUShaderStage.FRAGMENT;
      for (let e = 0; e < g.length; e += 8) {
        const [binding, kind, , vdim] = [g[e]!, g[e + 1]!, g[e + 2]!, g[e + 3]!];
        const viewDimension = VIEW_DIM[vdim] ?? "2d";
        if (kind === 0) entries.push({ binding, visibility, buffer: { type: "uniform" } });
        else if (kind === 2) entries.push({ binding, visibility, sampler: { type: "filtering" } });
        else if (kind === 3) entries.push({ binding, visibility, sampler: { type: "comparison" } });
        else {
          const sampleType: GPUTextureSampleType = kind === 4 ? "uint" : kind === 5 ? "sint" : kind === 6 ? "depth" : "float";
          entries.push({ binding, visibility, texture: { sampleType, viewDimension } });
        }
      }
      return this.device.createBindGroupLayout({ entries });
    });
    // key: crates/vnhost/src/d3d11 draw()
    let q = 0;
    const vs = k[q++]!,
      ps = k[q++]!,
      topo = k[q++]!,
      strip = k[q++]!;
    const cull = k[q++]!,
      ccw = k[q++]!,
      bias = k[q++]! | 0,
      slope = f32of(k[q++]!),
      clamp = f32of(k[q++]!);
    let depthStencil: GPUDepthStencilState | undefined;
    if (k[q] === 0xffff) q++;
    else {
      const [fmt, en, write, func, sen, masks] = Array.from(k.subarray(q, q + 6));
      const face = (o: number): GPUStencilFaceState =>
        sen ? { failOp: STENCIL_OP[k[o]!]!, depthFailOp: STENCIL_OP[k[o + 1]!]!, passOp: STENCIL_OP[k[o + 2]!]!, compare: COMPARE[k[o + 3]!]! } : {};
      const format = TEX_FORMATS[fmt!] ?? "depth24plus-stencil8";
      const tri = topo >= 3;
      depthStencil = {
        format,
        depthWriteEnabled: en !== 0 && write !== 0,
        depthCompare: en ? (COMPARE[func!] ?? "less") : "always",
        ...(format === "depth24plus-stencil8"
          ? { stencilFront: face(q + 6), stencilBack: face(q + 10), stencilReadMask: sen ? masks! & 0xff : 0xff, stencilWriteMask: sen ? (masks! >> 8) & 0xff : 0 }
          : {}),
        depthBias: tri ? bias : 0,
        depthBiasSlopeScale: tri ? slope : 0,
        depthBiasClamp: tri ? clamp : 0,
      };
      q += 14;
    }
    const buffers: GPUVertexBufferLayout[] = [];
    const nvb = k[q++]!;
    for (let b = 0; b < nvb; b++) {
      const stride = k[q++]!,
        step = k[q++]!,
        n = k[q++]!;
      const attributes: GPUVertexAttribute[] = [];
      for (let a = 0; a < n; a++, q += 3) attributes.push({ format: VERTEX_FORMATS2[k[q]!] ?? "float32x4", offset: k[q + 1]!, shaderLocation: k[q + 2]! });
      buffers.push({ arrayStride: Math.ceil(stride / 4) * 4, stepMode: step ? "instance" : "vertex", attributes });
    }
    const targets: GPUColorTargetState[] = [];
    const nt = k[q++]!;
    for (let t = 0; t < nt; t++, q += 9) {
      const [fmt, en, src, dst, op, srcA, dstA, opA, mask] = Array.from(k.subarray(q, q + 9)) as number[];
      const format = TEX_FORMATS[fmt!] ?? "bgra8unorm";
      const comp = (s: number, d: number, o: number): GPUBlendComponent => {
        const operation = BLEND_OP[o] ?? "add";
        return operation === "min" || operation === "max" ? { operation, srcFactor: "one", dstFactor: "one" } : { operation, srcFactor: BLEND2[s] ?? "one", dstFactor: BLEND2[d] ?? "zero" };
      };
      const blendable = !format.endsWith("uint") && !format.endsWith("sint") && format !== "r32float" && format !== "rg32float";
      targets.push({
        format,
        blend: en && blendable ? { color: comp(src!, dst!, op!), alpha: comp(srcA!, dstA!, opA!) } : undefined,
        writeMask: ps ? mask! : 0,
      });
    }
    const fragment = ps ? this.shaders.get(ps) : (this.noPs ??= this.device.createShaderModule({ code: "@fragment fn main() {}" }));
    const vertex = this.shaders.get(vs);
    if (!vertex || !fragment) return false;
    p.pipeline = this.device.createRenderPipeline({
      layout: this.device.createPipelineLayout({ bindGroupLayouts: layouts }),
      vertex: { module: vertex, entryPoint: "main", buffers },
      fragment: { module: fragment, entryPoint: "main", targets },
      primitive: {
        topology: TOPOLOGY[topo] ?? "triangle-list",
        stripIndexFormat: strip === 1 ? "uint16" : strip === 2 ? "uint32" : undefined,
        cullMode: cull === 2 ? "front" : cull === 3 ? "back" : "none",
        frontFace: ccw ? "ccw" : "cw",
      },
      depthStencil,
    });
    p.layouts = layouts;
    p.kinds = kinds;
    return true;
  }
  private noPs: GPUShaderModule | undefined;

  private draw2(c: Uint32Array, i: number): number {
    let q = i + 1;
    const p = this.pipelines2.get(c[q++]!);
    const f = new Float32Array(c.buffer, c.byteOffset + q * 4, 6);
    const [vx, vy, vw, vh, minz, maxz] = Array.from(f) as number[];
    q += 6;
    const [sx, sy, sw, sh, sOn] = Array.from(c.subarray(q, q + 5)) as number[];
    q += 5;
    const bf = new Float32Array(c.buffer, c.byteOffset + q * 4, 4);
    const blendConst = { r: bf[0]!, g: bf[1]!, b: bf[2]!, a: bf[3]! };
    q += 4;
    const stencilRef = c[q++]!;
    const nvb = c[q++]!;
    const vbs: [number, number][] = [];
    for (let b = 0; b < nvb; b++, q += 2) vbs.push([c[q]!, c[q + 1]!]);
    const [ib, ioff, ifmt] = [c[q]!, c[q + 1]!, c[q + 2]!];
    q += 3;
    const groups: Uint32Array[] = [];
    for (let g = 0; g < 2; g++) {
      const n = c[q++]!;
      groups.push(c.subarray(q, q + n * 8));
      q += n * 8;
    }
    const [indexed, count, instances, start, base, first] = Array.from(c.subarray(q, q + 6)) as number[];
    q += 6;
    if (!p || !count || !instances) return q;
    if (!this.pipeline2(p, groups)) return q;
    const pass = this.beginPass();
    if (!pass) return q;
    const [tw, th] = this.targetSize2();
    // viewport inside the target (WebGPU wants it there)
    let [x0, y0, x1, y1] = vw! > 0 && vh! > 0 ? [vx!, vy!, vx! + vw!, vy! + vh!] : [0, 0, tw, th];
    x0 = Math.max(0, x0);
    y0 = Math.max(0, y0);
    x1 = Math.min(tw, x1);
    y1 = Math.min(th, y1);
    if (x1 <= x0 || y1 <= y0) return q;
    const z0 = Math.min(1, Math.max(0, Math.min(minz!, maxz!))),
      z1 = Math.min(1, Math.max(0, Math.max(minz!, maxz!)));
    pass.setPipeline(p.pipeline!);
    pass.setViewport(x0, y0, x1 - x0, y1 - y0, z0, z1);
    let [rx0, ry0, rx1, ry1] = sOn ? [sx!, sy!, sx! + sw!, sy! + sh!] : [0, 0, tw, th];
    rx0 = Math.max(rx0, Math.floor(x0));
    ry0 = Math.max(ry0, Math.floor(y0));
    rx1 = Math.min(rx1, Math.ceil(x1), tw);
    ry1 = Math.min(ry1, Math.ceil(y1), th);
    if (rx1 <= rx0 || ry1 <= ry0) return q;
    pass.setScissorRect(rx0, ry0, rx1 - rx0, ry1 - ry0);
    pass.setBlendConstant(blendConst);
    pass.setStencilReference(stencilRef);
    groups.forEach((g, gi) => {
      const entries: GPUBindGroupEntry[] = [];
      for (let e = 0; e < g.length; e += 8) {
        const [binding, kind, a, b, cc, dd, ee, ff] = Array.from(g.subarray(e, e + 8)) as number[];
        if (kind === 0 && a === DATA) {
          entries.push({ binding: binding!, resource: { buffer: this.dataBuf!, offset: b!, size: Math.max(16, cc!) } });
        } else if (kind === 0) {
          const buf = this.buffers.get(a!);
          const want = Math.max(16, Math.ceil(dd! / 16) * 16);
          entries.push({ binding: binding!, resource: buf && buf.size >= want ? { buffer: buf, offset: 0, size: Math.min(buf.size, 65536) } : { buffer: this.zero(), offset: 0, size: Math.min(65536, want) } });
        } else if (kind === 2 || kind === 3) {
          entries.push({ binding: binding!, resource: this.sampler2(a!, b!, cc!) });
        } else {
          const t = this.textures.get(a!);
          const depth = t ? isDepth(t.format) : false;
          const ok = t && depth === (kind === 6) && (kind !== 1 || !t.format.match(/int$/)) && (kind !== 4 || t.format.endsWith("uint")) && (kind !== 5 || t.format.endsWith("sint"));
          if (ok) this.used.add(a!);
          entries.push({ binding: binding!, resource: ok ? this.view2(t, VIEW_DIM[b!] ?? "2d", cc!, dd!, ee!, ff!, depth ? "depth-only" : "all") : this.dummy(b!, kind!) });
        }
      }
      pass.setBindGroup(gi, this.device.createBindGroup({ layout: p.layouts![gi]!, entries }));
    });
    vbs.forEach(([b, off], k) => {
      const buf = b === DATA ? this.dataBuf : this.buffers.get(b);
      pass.setVertexBuffer(k, buf ?? this.zero(), buf ? Math.min(off, buf.size) : 0);
    });
    if (indexed) {
      const buf = ib === DATA ? this.dataBuf : this.buffers.get(ib);
      if (!buf) return q;
      pass.setIndexBuffer(buf, ifmt === 2 ? "uint32" : "uint16", Math.min(ioff, buf.size));
      pass.drawIndexed(count!, instances!, start!, base! | 0, first!);
    } else pass.draw(count!, instances!, start!, first!);
    this.passDraws++;
    this.draws++;
    return q;
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
    // (OpenGL frames: bottom row first)
    for (let r = 0; r < t.h; r++) data.set(src.subarray(r * bpr, r * bpr + t.w * 4), (o.flip ? t.h - 1 - r : r) * t.w * 4);
    if (t.format.startsWith("rgba8")) {
      // (as BGRA)
      for (let p = 0; p < data.length; p += 4) [data[p], data[p + 2]] = [data[p + 2]!, data[p]!];
    }
    buf.unmap();
    buf.destroy();
    return { w: t.w, h: t.h, data };
  }
}
