// The device benchmark's WebGPU step (web/src/bench.ts): what the phone's GPU does with the
// work ippai gives it, without any game: a 1280x720 picture blended LAYERS times into render
// targets of several sizes (a VN's layers; the bigger sizes are a high-DPI canvas), uploads of a
// 1280x720 frame (a movie or a CPU-drawn screen), and presenting to the canvas at the device's
// pixels and at CSS pixels (does drawing at devicePixelRatio cost frames?).

const SHADER = /* wgsl */ `
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;
struct V { @builtin(position) p: vec4f, @location(0) uv: vec2f };
@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
  let xy = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
  return V(vec4f(xy * 2.0 - 1.0, 0.0, 1.0), vec2f(xy.x, 1.0 - xy.y));
}
@fragment fn fs(v: V) -> @location(0) vec4f {
  let c = textureSample(t, s, v.uv);
  return vec4f(c.rgb, 0.5);
}`;

export const LAYERS = 8;
const FRAMES = 20;
const SRC = [1280, 720] as const;

/** The sizes the fill test draws at: 720p, 1080p and the canvas at the device's pixels. */
export function fillSizes(cssW: number, cssH: number, dpr: number, maxTexture: number): [number, number][] {
  const out: [number, number][] = [];
  const add = (w: number, h: number) => {
    w = Math.min(maxTexture, Math.max(1, Math.round(w)));
    h = Math.min(maxTexture, Math.max(1, Math.round(h)));
    if (!out.some(([a, b]) => a === w && b === h)) out.push([w, h]);
  };
  add(1280, 720);
  add(1920, 1080);
  // (landscape: games are)
  const [w, h] = cssW >= cssH ? [cssW, cssH] : [cssH, cssW];
  add(w * dpr, h * dpr);
  return out;
}

export async function gpuBench(
  canvas: HTMLCanvasElement,
  metric: (name: string, v: number) => void,
): Promise<void> {
  if (!navigator.gpu) throw new Error("WebGPU がありません");
  const adapter = await navigator.gpu.requestAdapter();
  if (!adapter) throw new Error("WebGPU のアダプターがありません");
  const device = await adapter.requestDevice();
  const format = "rgba8unorm";
  const module = device.createShaderModule({ code: SHADER });
  const pipe = (f: GPUTextureFormat) =>
    device.createRenderPipeline({
      layout: "auto",
      vertex: { module, entryPoint: "vs" },
      fragment: {
        module,
        entryPoint: "fs",
        targets: [
          {
            format: f,
            blend: {
              color: { srcFactor: "src-alpha", dstFactor: "one-minus-src-alpha" },
              alpha: { srcFactor: "one", dstFactor: "one-minus-src-alpha" },
            },
          },
        ],
      },
      primitive: { topology: "triangle-list" },
    });
  const pipeline = pipe(format);
  const src = device.createTexture({
    size: [SRC[0], SRC[1]],
    format,
    usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
  });
  const pixels = new Uint8Array(SRC[0] * SRC[1] * 4);
  for (let i = 0; i < pixels.length; i++) pixels[i] = (i * 2654435761) >>> 24;
  device.queue.writeTexture({ texture: src }, pixels, { bytesPerRow: SRC[0] * 4 }, [SRC[0], SRC[1]]);
  const sampler = device.createSampler({ magFilter: "linear", minFilter: "linear" });
  const group = (p: GPURenderPipeline) =>
    device.createBindGroup({
      layout: p.getBindGroupLayout(0),
      entries: [
        { binding: 0, resource: src.createView() },
        { binding: 1, resource: sampler },
      ],
    });
  const bind = group(pipeline);
  const draw = (view: GPUTextureView, p: GPURenderPipeline, g: GPUBindGroup, layers: number) => {
    const enc = device.createCommandEncoder();
    const pass = enc.beginRenderPass({
      colorAttachments: [{ view, loadOp: "clear", storeOp: "store", clearValue: [0, 0, 0, 1] }],
    });
    pass.setPipeline(p);
    pass.setBindGroup(0, g);
    for (let l = 0; l < layers; l++) pass.draw(3);
    pass.end();
    device.queue.submit([enc.finish()]);
  };
  await device.queue.onSubmittedWorkDone();

  // fill: ms per frame of LAYERS blended full-screen layers, per size
  const sizes = fillSizes(
    innerWidth,
    innerHeight,
    devicePixelRatio || 1,
    device.limits.maxTextureDimension2D,
  );
  let best = 0;
  for (const [w, h] of sizes) {
    const rt = device.createTexture({ size: [w, h], format, usage: GPUTextureUsage.RENDER_ATTACHMENT });
    const view = rt.createView();
    draw(view, pipeline, bind, LAYERS);
    await device.queue.onSubmittedWorkDone();
    const t = performance.now();
    for (let f = 0; f < FRAMES; f++) draw(view, pipeline, bind, LAYERS);
    await device.queue.onSubmittedWorkDone();
    const ms = (performance.now() - t) / FRAMES;
    metric(`fill_${w}x${h}.ms`, ms);
    best = Math.max(best, (w * h * LAYERS) / (ms * 1000));
    rt.destroy();
  }
  metric("fill.mpix_s", best);

  // uploads of a whole 1280x720 frame
  {
    const n = 30;
    const t = performance.now();
    for (let i = 0; i < n; i++)
      device.queue.writeTexture({ texture: src }, pixels, { bytesPerRow: SRC[0] * 4 }, [SRC[0], SRC[1]]);
    await device.queue.onSubmittedWorkDone();
    metric("upload.mb_s", (pixels.length * n) / 1e6 / ((performance.now() - t) / 1000));
  }

  // presenting: frame interval with one layer drawn to the canvas, at device and CSS pixels
  const ctx = canvas.getContext("webgpu");
  if (ctx) {
    const cf = navigator.gpu.getPreferredCanvasFormat();
    const p = pipe(cf);
    const g = group(p);
    for (const [name, scale] of [
      ["present_dpr.ms", devicePixelRatio || 1],
      ["present_1x.ms", 1],
    ] as const) {
      canvas.width = Math.max(1, Math.round(canvas.clientWidth * scale));
      canvas.height = Math.max(1, Math.round(canvas.clientHeight * scale));
      ctx.configure({ device, format: cf, alphaMode: "opaque" });
      const times: number[] = [];
      await new Promise<void>((done) => {
        let last = 0;
        const frame = (now: number) => {
          draw(ctx.getCurrentTexture().createView(), p, g, LAYERS);
          if (last) times.push(now - last);
          last = now;
          if (times.length < 60) requestAnimationFrame(frame);
          else done();
        };
        requestAnimationFrame(frame);
      });
      times.sort((a, b) => a - b);
      metric(name, times[times.length >> 1]!);
      ctx.unconfigure();
    }
  }
  src.destroy();
  device.destroy();
}
