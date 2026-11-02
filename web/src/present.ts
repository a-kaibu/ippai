// Shows the guest framebuffer on the page's canvas with WebGPU, letterboxed.
// Only the rows reported dirty are uploaded each frame. Wine's screen can be cropped to a
// part of it (`view`: the game window's client area, without title bar or desktop).

const SHADER = /* wgsl */ `
struct Out { @builtin(position) position: vec4f, @location(0) uv: vec2f };

@vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
  let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
  var out: Out;
  out.position = vec4f(p * 2.0 - 1.0, 0.0, 1.0);
  out.uv = mix(uv.xy, uv.zw, vec2f(p.x, 1.0 - p.y));
  return out;
}

@group(0) @binding(0) var frame: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
/** the part of the texture drawn: u0 v0 u1 v1 */
@group(0) @binding(2) var<uniform> uv: vec4f;

@fragment fn fs(in: Out) -> @location(0) vec4f {
  return vec4f(textureSample(frame, smp, in.uv).rgb, 1.0);
}
`;

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Where a frame of `width`×`height` goes on a canvas of `cw`×`ch` device pixels. */
export function letterbox(cw: number, ch: number, width: number, height: number): Rect {
  let scale = Math.min(cw / width, ch / height);
  const integer = Math.floor(scale);
  if (integer >= 1 && integer / scale > 0.92) scale = integer;
  const w = Math.round(width * scale);
  const h = Math.round(height * scale);
  return { x: Math.floor((cw - w) / 2), y: Math.floor((ch - h) / 2), w, h };
}

export class Presenter {
  width = 1;
  height = 1;
  /** the part of the frame shown (l t r b in frame pixels); null: all of it */
  view: [number, number, number, number] | null = null;
  private shown: [number, number, number, number] = [0, 0, 1, 1];
  private readonly uvMain: GPUBuffer;
  private readonly uvOverlay: GPUBuffer;
  /** Direct3D windows drawn over the screen (web/src/d3d9.ts) */
  overlay: { view: GPUTextureView; rect: [number, number, number, number]; flip?: boolean } | null = null;
  private overlayGroup: { view: GPUTextureView; group: GPUBindGroup } | null = null;
  /** windows above the Direct3D window (frame pixels l t r b): drawn from the frame over it */
  above: [number, number, number, number][] = [];
  private uvAbove: GPUBuffer[] = [];
  private texView: GPUTextureView | null = null;
  private rect: Rect = { x: 0, y: 0, w: 1, h: 1 };
  private texture: GPUTexture | null = null;
  private bindGroup: GPUBindGroup | null = null;
  // WebGPU may refuse views on SharedArrayBuffer; then rows are copied here first.
  private staging: Uint8Array<ArrayBuffer> | null = null;
  private sharedOk = true;

  private constructor(
    readonly canvas: HTMLCanvasElement,
    readonly adapter: GPUAdapter,
    readonly device: GPUDevice,
    private readonly context: GPUCanvasContext,
    private readonly pipeline: GPURenderPipeline,
    private readonly samplers: { nearest: GPUSampler; linear: GPUSampler },
  ) {
    const uv = () =>
      device.createBuffer({ size: 16, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
    this.uvMain = uv();
    this.uvOverlay = uv();
  }

  static async create(canvas: HTMLCanvasElement): Promise<Presenter> {
    const adapter = await navigator.gpu?.requestAdapter();
    if (!adapter) throw new Error("WebGPU が使えません");
    // (Direct3D 11 games sample 32-bit float textures with linear filters)
    const want: GPUFeatureName[] = ["float32-filterable", "rg11b10ufloat-renderable"];
    const device = await adapter.requestDevice({
      requiredFeatures: want.filter((f) => adapter.features.has(f)),
    });
    const context = canvas.getContext("webgpu")!;
    const format = navigator.gpu.getPreferredCanvasFormat();
    context.configure({ device, format, alphaMode: "opaque" });
    const module = device.createShaderModule({ code: SHADER });
    const pipeline = device.createRenderPipeline({
      layout: "auto",
      vertex: { module, entryPoint: "vs" },
      fragment: { module, entryPoint: "fs", targets: [{ format }] },
      primitive: { topology: "triangle-list" },
    });
    const samplers = {
      nearest: device.createSampler({ magFilter: "nearest", minFilter: "nearest" }),
      linear: device.createSampler({ magFilter: "linear", minFilter: "linear" }),
    };
    void device.lost.then((info) => console.error(`WebGPU device lost (${info.reason}): ${info.message}`));
    device.addEventListener("uncapturederror", (e) =>
      console.error(`WebGPU: ${(e as GPUUncapturedErrorEvent).error.message}`),
    );
    return new Presenter(canvas, adapter, device, context, pipeline, samplers);
  }

  /** The frame pixel under a pointer event, clamped to the frame. */
  toFrame(event: { clientX: number; clientY: number }): [number, number] {
    const bounds = this.canvas.getBoundingClientRect();
    const dpr = this.canvas.width / Math.max(1, bounds.width);
    const r = this.rect;
    const [l, t, vr, vb] = this.shown;
    const x = l + Math.floor((((event.clientX - bounds.left) * dpr - r.x) * (vr - l)) / Math.max(1, r.w));
    const y = t + Math.floor((((event.clientY - bounds.top) * dpr - r.y) * (vb - t)) / Math.max(1, r.h));
    return [Math.max(0, Math.min(this.width - 1, x)), Math.max(0, Math.min(this.height - 1, y))];
  }

  private layout(width: number, height: number): boolean {
    const dpr = window.devicePixelRatio || 1;
    const cw = Math.max(1, Math.floor(this.canvas.clientWidth * dpr));
    const ch = Math.max(1, Math.floor(this.canvas.clientHeight * dpr));
    const v =
      this.view && this.view[2] > this.view[0] && this.view[3] > this.view[1]
        ? this.view
        : [0, 0, width, height];
    let changed = width !== this.width || height !== this.height || v.some((x, i) => x !== this.shown[i]);
    if (this.canvas.width !== cw || this.canvas.height !== ch) {
      this.canvas.width = cw;
      this.canvas.height = ch;
      changed = true;
    }
    this.width = width;
    this.height = height;
    this.shown = [v[0]!, v[1]!, v[2]!, v[3]!];
    this.rect = letterbox(cw, ch, v[2]! - v[0]!, v[3]! - v[1]!);
    if (changed)
      this.device.queue.writeBuffer(
        this.uvMain,
        0,
        new Float32Array([v[0]! / width, v[1]! / height, v[2]! / width, v[3]! / height]),
      );
    return changed;
  }

  /**
   * Uploads rows [y0, y1) of a BGRX frame that lives in `heap` at `offset`
   * (`stride` bytes per row) and draws it.
   */
  update(
    heap: Uint8Array,
    offset: number,
    width: number,
    height: number,
    stride: number,
    y0: number,
    y1: number,
  ): void {
    if (!width || !height) return;
    const resized = this.layout(width, height);
    const nearest =
      this.rect.w % (this.shown[2] - this.shown[0]) === 0 &&
      this.rect.h % (this.shown[3] - this.shown[1]) === 0;
    if (!this.texture || this.texture.width !== width || this.texture.height !== height || resized) {
      this.texture?.destroy();
      this.texture = this.device.createTexture({
        size: [width, height],
        format: "bgra8unorm",
        usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
      });
      this.texView = this.texture.createView();
      this.bindGroup = this.device.createBindGroup({
        layout: this.pipeline.getBindGroupLayout(0),
        entries: [
          { binding: 0, resource: this.texView },
          { binding: 1, resource: nearest ? this.samplers.nearest : this.samplers.linear },
          { binding: 2, resource: { buffer: this.uvMain } },
        ],
      });
      y0 = 0;
      y1 = height;
    }
    if (y1 > y0) this.upload(heap, offset, width, stride, y0, y1);
    this.draw();
  }

  /**
   * As `update`, rows [y0, y1) taken from a window's own surface (`ptr`, `stride` pixels a row)
   * whose rect on the frame is l t r b: only that rect's part of them changed.
   */
  updateFrom(
    heap: Uint8Array,
    width: number,
    height: number,
    win: { ptr: number; stride: number; l: number; t: number; r: number; b: number },
    y0: number,
    y1: number,
  ): boolean {
    if (!width || !height) return true;
    if (!this.texture || this.texture.width !== width || this.texture.height !== height) return false;
    if (this.layout(width, height)) return false;
    const x0 = Math.max(0, win.l),
      x1 = Math.min(width, win.r);
    const t = Math.max(y0, win.t, 0),
      b = Math.min(y1, win.b, height);
    if (x1 > x0 && b > t) {
      const off = win.ptr + ((t - win.t) * win.stride + (x0 - win.l)) * 4;
      const bpr = win.stride * 4;
      const rows = heap.subarray(off, off + (b - t - 1) * bpr + (x1 - x0) * 4);
      const dst = { texture: this.texture, origin: { x: x0, y: t } };
      const size = [x1 - x0, b - t];
      if (this.sharedOk) {
        try {
          this.device.queue.writeTexture(dst, rows as Uint8Array<ArrayBuffer>, { bytesPerRow: bpr }, size);
        } catch {
          this.sharedOk = false;
        }
      }
      if (!this.sharedOk) {
        if (!this.staging || this.staging.length < rows.length) this.staging = new Uint8Array(rows.length);
        this.staging.set(rows);
        this.device.queue.writeTexture(
          dst,
          this.staging.subarray(0, rows.length),
          { bytesPerRow: bpr },
          size,
        );
      }
    }
    this.draw();
    return true;
  }

  private upload(
    heap: Uint8Array,
    offset: number,
    width: number,
    stride: number,
    y0: number,
    y1: number,
  ): void {
    const rows = heap.subarray(offset + y0 * stride, offset + y1 * stride);
    const dst = { texture: this.texture!, origin: { x: 0, y: y0 } };
    const size = [width, y1 - y0];
    if (this.sharedOk) {
      try {
        this.device.queue.writeTexture(dst, rows as Uint8Array<ArrayBuffer>, { bytesPerRow: stride }, size);
        return;
      } catch {
        this.sharedOk = false;
      }
    }
    if (!this.staging || this.staging.length < rows.length) this.staging = new Uint8Array(rows.length);
    this.staging.set(rows);
    this.device.queue.writeTexture(dst, this.staging, { bytesPerRow: stride }, size);
  }

  /** Headless Chromium destroys a device that draws to a canvas: tests set this (&headless). */
  headless = false;

  draw(): void {
    if (!this.bindGroup || this.headless) return;
    // (the canvas or the view may have changed without a new frame)
    this.layout(this.width, this.height);
    const encoder = this.device.createCommandEncoder();
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view: this.context.getCurrentTexture().createView(),
          loadOp: "clear",
          storeOp: "store",
          clearValue: { r: 0, g: 0, b: 0, a: 1 },
        },
      ],
    });
    const r = this.rect;
    pass.setViewport(r.x, r.y, r.w, r.h, 0, 1);
    pass.setPipeline(this.pipeline);
    pass.setBindGroup(0, this.bindGroup);
    pass.draw(3);
    const o = this.overlay;
    if (o) {
      if (this.overlayGroup?.view !== o.view) {
        this.overlayGroup = {
          view: o.view,
          group: this.device.createBindGroup({
            layout: this.pipeline.getBindGroupLayout(0),
            entries: [
              { binding: 0, resource: o.view },
              { binding: 1, resource: this.samplers.linear },
              { binding: 2, resource: { buffer: this.uvOverlay } },
            ],
          }),
        };
      }
      // the window's client area within the view, from frame pixels to the canvas
      const [vl, vt, vr, vb] = this.shown;
      const [ol, ot, or, ob] = o.rect;
      const l = Math.max(ol, vl),
        t = Math.max(ot, vt),
        rr = Math.min(or, vr),
        b = Math.min(ob, vb);
      const sx = r.w / (vr - vl),
        sy = r.h / (vb - vt);
      const x0 = r.x + (l - vl) * sx,
        y0 = r.y + (t - vt) * sy;
      const x1 = Math.min(this.canvas.width, r.x + (rr - vl) * sx),
        y1 = Math.min(this.canvas.height, r.y + (b - vt) * sy);
      if (x1 > x0 && y1 > y0 && or > ol && ob > ot) {
        const uv = [(l - ol) / (or - ol), (t - ot) / (ob - ot), (rr - ol) / (or - ol), (b - ot) / (ob - ot)];
        // (OpenGL frames are stored bottom row first)
        if (o.flip) [uv[1], uv[3]] = [1 - uv[1]!, 1 - uv[3]!];
        this.device.queue.writeBuffer(this.uvOverlay, 0, new Float32Array(uv));
        pass.setViewport(x0, y0, x1 - x0, y1 - y0, 0, 1);
        pass.setBindGroup(0, this.overlayGroup.group);
        pass.draw(3);
      }
      // dialogs and other windows above it
      const [vl2, vt2, vr2, vb2] = this.shown;
      this.above.slice(0, 8).forEach(([al, at, ar, ab], k) => {
        const l = Math.max(al, vl2),
          t = Math.max(at, vt2),
          rr = Math.min(ar, vr2),
          b = Math.min(ab, vb2);
        if (rr <= l || b <= t || !this.texView) return;
        const sx = r.w / (vr2 - vl2),
          sy = r.h / (vb2 - vt2);
        const x0 = r.x + (l - vl2) * sx,
          y0 = r.y + (t - vt2) * sy;
        const x1 = Math.min(this.canvas.width, r.x + (rr - vl2) * sx),
          y1 = Math.min(this.canvas.height, r.y + (b - vt2) * sy);
        if (x1 <= x0 || y1 <= y0) return;
        this.uvAbove[k] ??= this.device.createBuffer({
          size: 16,
          usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
        });
        this.device.queue.writeBuffer(
          this.uvAbove[k]!,
          0,
          new Float32Array([l / this.width, t / this.height, rr / this.width, b / this.height]),
        );
        const group = this.device.createBindGroup({
          layout: this.pipeline.getBindGroupLayout(0),
          entries: [
            { binding: 0, resource: this.texView },
            { binding: 1, resource: this.samplers.nearest },
            { binding: 2, resource: { buffer: this.uvAbove[k]! } },
          ],
        });
        pass.setViewport(x0, y0, x1 - x0, y1 - y0, 0, 1);
        pass.setBindGroup(0, group);
        pass.draw(3);
      });
    }
    pass.end();
    this.device.queue.submit([encoder.finish()]);
  }
}
