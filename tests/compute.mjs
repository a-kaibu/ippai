// Compute shaders on WebGPU, for differential tests: runs each case's WGSL variants (the same
// shader through different translators) on the same inputs in headless Chromium (Dawn;
// SwiftShader unless CHROME_ARGS picks another adapter) and compares what each one stored with
// the first variant's: every byte within the case's `tol` (default 0). Exits 1 when a variant
// fails or differs.
//
//   cargo run -q -p shaderconf -- --compute > cases.json && node tests/compute.mjs cases.json [out.json]
//   (PLAYWRIGHT=path to playwright)
//
// cases.json: [{ name, dispatch: [x, y, z], out: binding, tol?, want?: [bytes], variants: [{ name, wgsl, entry? }],
//   bindings: [{ binding, uniform: [u32...] } | { binding, texture: { format, w, h, bytes: [..]
//   | f32: [..] } } | { binding, sampler: { filter, address } } | { binding, storage: { format, w,
//   h } }] }]
// out.json: [{ name, variants: [{ name, error?, bytes: [..] }] }] (the storage texture `out`)
import { createServer } from "node:http";
import { readFileSync, writeFileSync } from "node:fs";

const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const chromium = pw.chromium ?? pw.default.chromium;
const [file, outFile] = process.argv.slice(2);
const cases = JSON.parse(readFileSync(file, "utf8"));

// (WebGPU wants a secure context: a page on localhost)
const server = createServer((_, res) => {
  res.writeHead(200, { "content-type": "text/html" });
  res.end("<!doctype html><title>compute</title>");
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const args = ["--enable-unsafe-webgpu", "--use-webgpu-adapter=swiftshader", "--enable-features=Vulkan",
  ...(process.env.CHROME_ARGS ?? "").split(" ").filter(Boolean)];
const browser = await chromium.launch({ args });
const page = await browser.newPage();
page.on("console", (m) => m.type() === "error" && console.error("page:", m.text()));
await page.goto(`http://127.0.0.1:${server.address().port}/`);

const results = [];
let bad = 0;
const n_ok = (what) => (what.startsWith("ok") ? "ok  " : "FAIL");
for (const c of cases) {
  const r = await page.evaluate(async (c) => {
    const adapter = await navigator.gpu.requestAdapter();
    // (Direct3D 11 gives a thread group 32 KB of shared memory and 1024 threads; WebGPU's
    // defaults are 16 KB and 256: what the adapter allows, as the page asks for)
    const L = adapter.limits;
    const device = await adapter.requestDevice({ requiredLimits: {
      maxComputeWorkgroupStorageSize: L.maxComputeWorkgroupStorageSize,
      maxComputeInvocationsPerWorkgroup: L.maxComputeInvocationsPerWorkgroup,
      maxComputeWorkgroupSizeX: L.maxComputeWorkgroupSizeX,
      maxComputeWorkgroupSizeY: L.maxComputeWorkgroupSizeY,
      maxComputeWorkgroupSizeZ: L.maxComputeWorkgroupSizeZ,
    } });
    const bpp = (f) => ({ rgba8unorm: 4, rgba8snorm: 4, bgra8unorm: 4, rgba16float: 8, rgba32float: 16, r32float: 4, rg32float: 8, rgba32uint: 16, r32uint: 4 })[f];
    const res = {};
    for (const b of c.bindings) {
      if (b.uniform) {
        const buf = device.createBuffer({ size: Math.max(16, Math.ceil(b.uniform.length * 4 / 16) * 16), usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
        device.queue.writeBuffer(buf, 0, new Uint32Array(b.uniform));
        res[b.binding] = { buffer: buf };
      } else if (b.texture) {
        const t = b.texture;
        const tex = device.createTexture({ size: [t.w, t.h], format: t.format, usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST });
        const data = t.f32 ? new Float32Array(t.f32) : new Uint8Array(t.bytes);
        device.queue.writeTexture({ texture: tex }, data, { bytesPerRow: t.w * bpp(t.format) }, [t.w, t.h]);
        res[b.binding] = tex.createView();
      } else if (b.sampler) {
        const f = b.sampler.filter ?? "linear", a = b.sampler.address ?? "clamp-to-edge";
        res[b.binding] = device.createSampler({ magFilter: f, minFilter: f, mipmapFilter: f, addressModeU: a, addressModeV: a });
      } else if (b.storage) {
        const t = b.storage;
        const tex = device.createTexture({ size: [t.w, t.h], format: t.format, usage: GPUTextureUsage.STORAGE_BINDING | GPUTextureUsage.COPY_SRC | GPUTextureUsage.COPY_DST });
        // (texels a shader leaves alone: a pattern both variants keep)
        const fill = new Uint8Array(t.w * t.h * bpp(t.format)).map((_, i) => (i * 7 + 3) & 0xff);
        device.queue.writeTexture({ texture: tex }, fill, { bytesPerRow: t.w * bpp(t.format) }, [t.w, t.h]);
        res[b.binding] = { tex, view: tex.createView(), t };
      }
    }
    const out = [];
    for (const v of c.variants) {
      const o = { name: v.name };
      out.push(o);
      try {
        device.pushErrorScope("validation");
        const mod = device.createShaderModule({ code: v.wgsl });
        const ci = await mod.getCompilationInfo();
        const errs = ci.messages.filter((m) => m.type === "error");
        if (errs.length) {
          o.error = "WGSL: " + errs.map((m) => `${m.lineNum}:${m.linePos} ${m.message}`).join("; ");
          await device.popErrorScope();
          continue;
        }
        const pipe = device.createComputePipeline({ layout: "auto", compute: { module: mod, entryPoint: v.entry ?? "main" } });
        // (auto layouts hold what the entry point uses: bind those)
        const entries = [];
        for (const b of c.bindings) {
          if (!new RegExp(`@binding\\(${b.binding}\\)\\s*@group\\(0\\)|@group\\(0\\)\\s*@binding\\(${b.binding}\\)`).test(v.wgsl)) continue;
          const x = res[b.binding];
          entries.push({ binding: b.binding, resource: b.storage ? x.view : x });
        }
        let why = "";
        const tryGroup = async (es) => {
          device.pushErrorScope("validation");
          const g = device.createBindGroup({ layout: pipe.getBindGroupLayout(0), entries: es });
          const e = await device.popErrorScope();
          if (e && !why) why = e.message;
          return e ? null : g;
        };
        // (a binding the entry point does not use is not in the layout: left out one by one)
        let group = await tryGroup(entries);
        for (let k = entries.length - 1; !group && k >= 0; k--) {
          const es = entries.filter((_, j) => j !== k);
          const g = await tryGroup(es);
          if (g) { group = g; entries.splice(k, 1); }
        }
        if (!group) throw new Error("no bind group fits the layout: " + why);
        const st = res[c.out];
        const fill = new Uint8Array(st.t.w * st.t.h * bpp(st.t.format)).map((_, i) => (i * 7 + 3) & 0xff);
        device.queue.writeTexture({ texture: st.tex }, fill, { bytesPerRow: st.t.w * bpp(st.t.format) }, [st.t.w, st.t.h]);
        const enc = device.createCommandEncoder();
        const pass = enc.beginComputePass();
        pass.setPipeline(pipe);
        pass.setBindGroup(0, group);
        pass.dispatchWorkgroups(...c.dispatch);
        pass.end();
        const row = Math.ceil(st.t.w * bpp(st.t.format) / 256) * 256;
        const rb = device.createBuffer({ size: row * st.t.h, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
        enc.copyTextureToBuffer({ texture: st.tex }, { buffer: rb, bytesPerRow: row }, [st.t.w, st.t.h]);
        device.queue.submit([enc.finish()]);
        await rb.mapAsync(GPUMapMode.READ);
        const all = new Uint8Array(rb.getMappedRange());
        const w = st.t.w * bpp(st.t.format);
        const bytes = [];
        for (let y = 0; y < st.t.h; y++) for (let x = 0; x < w; x++) bytes.push(all[y * row + x]);
        rb.unmap();
        o.bytes = bytes;
        const e = await device.popErrorScope();
        if (e) o.error = "validation: " + e.message;
      } catch (e) {
        o.error = String(e);
      }
    }
    return { name: c.name, variants: out };
  }, c);
  results.push(r);
  // (against the bytes the case wants, or the first variant's)
  const first = c.want ? { name: "the answer", bytes: c.want } : r.variants[0];
  for (const v of r.variants) {
    let what = v.error ?? "ok";
    if (!v.error && v !== first && first.bytes) {
      let n = 0, max = 0, at = -1;
      v.bytes.forEach((b, i) => {
        const d = Math.abs(b - first.bytes[i]);
        if (d > (c.tol ?? 0)) n++, at < 0 && (at = i);
        max = Math.max(max, d);
      });
      what = n ? `${n} of ${v.bytes.length} bytes differ from ${first.name}'s (first at ${at}: ${v.bytes[at]} vs ${first.bytes[at]})` : `ok (within ${max} of ${first.name}'s)`;
      if (n) bad++;
    }
    if (v.error) bad++;
    console.log(`${n_ok(what)} ${c.name} / ${v.name}: ${what}`);
  }
}
if (outFile) writeFileSync(outFile, JSON.stringify(results));
await browser.close();
server.close();
console.log(`${bad ? "FAIL" : "ok"}: ${cases.length} cases`);
process.exit(bad ? 1 : 0);
