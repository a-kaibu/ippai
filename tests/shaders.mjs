// Shader conformance on WebGPU: runs the cases crates/shaderconf writes (each a pixel shader one
// of ippai's translators made, the uniform data of each pixel, and what the Direct3D / OpenGL
// rules say the pixel must be) in headless Chromium (Dawn; SwiftShader unless CHROME_ARGS picks
// another adapter), one draw per pixel into an rgba32float target, and compares.
//
//   cargo run -q -p shaderconf > cases.json && node tests/shaders.mjs cases.json [filter]
//   (PLAYWRIGHT=path to playwright; OUT=report.json; exits 1 when a case fails)
import { createServer } from "node:http";
import { readFileSync, writeFileSync } from "node:fs";

const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const chromium = pw.chromium ?? pw.default.chromium;
const [file, filter] = process.argv.slice(2);
let cases = JSON.parse(readFileSync(file, "utf8"));
if (filter) cases = cases.filter((c) => c.name.includes(filter));

// (WebGPU wants a secure context: a page on localhost)
const server = createServer((_, res) => {
  res.writeHead(200, { "content-type": "text/html" });
  res.end("<!doctype html><title>shaders</title>");
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const args = ["--enable-unsafe-webgpu", "--use-webgpu-adapter=swiftshader", "--enable-features=Vulkan",
  ...(process.env.CHROME_ARGS ?? "").split(" ").filter(Boolean)];
const browser = await chromium.launch({ args });
const page = await browser.newPage();
page.on("console", (m) => m.type() === "error" && console.error("page:", m.text()));
await page.goto(`http://127.0.0.1:${server.address().port}/`);

const results = await page.evaluate(async (cases) => {
  const adapter = await navigator.gpu.requestAdapter();
  const device = await adapter.requestDevice();
  const info = adapter.info ?? {};
  const vs = device.createShaderModule({
    code: "@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {\n" +
      "  let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));\n  return vec4f(p * 2.0 - 1.0, 0.5, 1.0);\n}\n",
  });
  const f32 = new Float32Array(1), u32 = new Uint32Array(f32.buffer);
  const bits = (x) => { f32[0] = x; return u32[0]; };
  const fromBits = (b) => { u32[0] = b; return f32[0]; };
  // floats as ordered integers (for ULP distances; -0 and +0 alike)
  const ord = (x) => { const b = bits(x); return b & 0x80000000 ? -(b & 0x7fffffff) : b; };
  const close = (got, want, tol) => {
    if (Number.isNaN(want)) return Number.isNaN(got);
    if (Number.isNaN(got)) return false;
    if (got === want) return true; // (+0 == -0, inf == inf)
    // (Direct3D and WebGPU may flush denormal results to zero)
    if (got === 0 && Math.abs(want) < 1.1754943508222875e-38) return true;
    if (!Number.isFinite(want) || !Number.isFinite(got)) return false;
    switch (tol[0]) {
      case "ulp": return Math.abs(ord(got) - ord(want)) <= tol[1];
      case "abs": return Math.abs(got - want) <= tol[1];
      case "rel": return Math.abs(got - want) <= Math.max(tol[1] * Math.abs(want), tol[2]);
      default: return false;
    }
  };
  const out = [];
  for (const c of cases) {
    const r = { name: c.name, rule: c.rule, pixels: c.pixels.length, failed: [] };
    out.push(r);
    try {
      device.pushErrorScope("validation");
      const fs = device.createShaderModule({ code: c.wgsl });
      const ci = await fs.getCompilationInfo();
      const errs = ci.messages.filter((m) => m.type === "error");
      if (errs.length) { r.error = "WGSL: " + errs.map((m) => `${m.lineNum}:${m.linePos} ${m.message}`).join("; "); await device.popErrorScope(); continue; }
      const pipe = device.createRenderPipeline({
        layout: "auto",
        vertex: { module: vs, entryPoint: "vs" },
        fragment: { module: fs, entryPoint: "main", targets: [{ format: "rgba32float" }] },
        primitive: { topology: "triangle-list" },
      });
      const w = c.pixels.length;
      const tex = device.createTexture({ size: [w, 1], format: "rgba32float", usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC });
      const enc = device.createCommandEncoder();
      const pass = enc.beginRenderPass({ colorAttachments: [{ view: tex.createView(), loadOp: "clear", storeOp: "store", clearValue: [-7, -7, -7, -7] }] });
      pass.setPipeline(pipe);
      const empty = [];
      for (let g = 0; g < c.group; g++) {
        empty.push(device.createBindGroup({ layout: pipe.getBindGroupLayout(g), entries: [] }));
        pass.setBindGroup(g, empty[g]);
      }
      const bufs = [];
      c.pixels.forEach((p, x) => {
        const data = new Uint32Array(Math.max(4, Math.ceil(p.data.length / 4) * 4));
        data.set(p.data);
        const b = device.createBuffer({ size: data.byteLength, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
        device.queue.writeBuffer(b, 0, data);
        bufs.push(b);
        pass.setBindGroup(c.group, device.createBindGroup({ layout: pipe.getBindGroupLayout(c.group), entries: [{ binding: c.binding, resource: { buffer: b } }] }));
        pass.setViewport(x, 0, 1, 1, 0, 1);
        pass.setScissorRect(x, 0, 1, 1);
        pass.draw(3);
      });
      pass.end();
      const bpr = Math.ceil(w * 16 / 256) * 256;
      const rb = device.createBuffer({ size: bpr, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
      enc.copyTextureToBuffer({ texture: tex }, { buffer: rb, bytesPerRow: bpr }, [w, 1]);
      device.queue.submit([enc.finish()]);
      const verr = await device.popErrorScope();
      if (verr) { r.error = "WebGPU: " + verr.message; continue; }
      await rb.mapAsync(GPUMapMode.READ);
      const px = new Float32Array(rb.getMappedRange().slice(0));
      rb.unmap();
      c.pixels.forEach((p, x) => {
        for (let k = 0; k < 4; k++) {
          const want = p.want[k];
          if (!want) continue;
          const got = px[x * 4 + k];
          if (!want.some((wb) => close(got, fromBits(wb), c.tol))) {
            r.failed.push({ pixel: x, channel: "xyzw"[k], got, gotBits: bits(got) >>> 0, want: want.map(fromBits), data: p.data.slice(0, 12) });
          }
        }
      });
      bufs.forEach((b) => b.destroy());
      tex.destroy();
      rb.destroy();
    } catch (e) {
      r.error = String(e);
    }
  }
  return { adapter: `${info.vendor ?? ""} ${info.architecture ?? ""}`.trim(), out };
}, cases);

await browser.close();
server.close();
let bad = 0;
const fmt = (x) => (Number.isNaN(x) ? "NaN" : Object.is(x, -0) ? "-0" : String(x));
console.log(`adapter: ${results.adapter}`);
for (const r of results.out) {
  if (r.error || r.failed.length) {
    bad++;
    console.log(`FAIL ${r.name} (${r.rule})${r.error ? ": " + r.error : `: ${r.failed.length} of ${r.pixels} pixels`}`);
    for (const f of r.failed.slice(0, 6)) {
      const inp = f.data.filter((_, i) => i % 4 === 0).map((w) => "0x" + (w >>> 0).toString(16)).join(" ");
      console.log(`     pixel ${f.pixel}.${f.channel}: got ${fmt(f.got)}, want ${f.want.map(fmt).join(" or ")} (inputs ${inp})`);
    }
  } else {
    console.log(`ok   ${r.name} (${r.pixels})`);
  }
}
console.log(`${results.out.length - bad} of ${results.out.length} cases pass`);
if (process.env.OUT) writeFileSync(process.env.OUT, JSON.stringify(results, null, 1));
process.exit(bad ? 1 : 0);
