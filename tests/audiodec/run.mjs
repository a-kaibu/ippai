// WebCodecs' AudioDecoder against crates/vorbis (wasm) on the same streams, in Chromium:
// decode speed (ms per second of sound), the page thread's share, a packet's wait when each is
// waited for, how far apart the samples are, and which codecs AudioDecoder takes.
//
//   node tests/audiodec/run.mjs WASM FILE...   (WASM: tests/audiodec/vorbis-wasm built for
//   wasm32-unknown-unknown; FILE: .ogg Vorbis/Opus, .opus, .mp3)
import { createServer } from "node:http";
import { readFileSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const chromium = pw.chromium ?? pw.default.chromium;

const here = dirname(fileURLToPath(import.meta.url));
const [wasmPath, ...files] = process.argv.slice(2);
if (!wasmPath || !files.length) {
  console.error("usage: run.mjs WASM FILE...");
  process.exit(2);
}
const ROUNDS = Number(process.env.ROUNDS ?? 5);

const server = createServer((req, res) => {
  const u = decodeURIComponent(new URL(req.url, "http://x").pathname);
  let body;
  let type = "application/octet-stream";
  if (u === "/") {
    body = '<!doctype html><meta charset="utf-8"><title>audiodec</title>';
    type = "text/html";
  } else if (u === "/page.js") {
    body = readFileSync(join(here, "page.js"));
    type = "text/javascript";
  } else if (u === "/dec.wasm") {
    body = readFileSync(wasmPath);
    type = "application/wasm";
  } else if (u.startsWith("/f/")) {
    body = readFileSync(files[Number(u.slice(3))]);
  }
  if (!body) {
    res.writeHead(404).end();
    return;
  }
  res.writeHead(200, { "content-type": type }).end(body);
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const base = `http://127.0.0.1:${server.address().port}`;

const browser = await chromium.launch({ args: ["--autoplay-policy=no-user-gesture-required"] });
const page = await browser.newPage();
page.on("console", (m) => console.error("page:", m.text()));
await page.goto(base + "/");
const cdp = await browser.newBrowserCDPSession();
/** CPU seconds of every Chromium process */
async function cpu() {
  const { processInfo } = await cdp.send("SystemInfo.getProcessInfo");
  return processInfo.reduce((a, p) => a + p.cpuTime, 0);
}

console.log(
  "AudioDecoder support:",
  JSON.stringify(
    await page.evaluate(async (files) => {
      const m = await import("/page.js");
      // a Vorbis stream's own config (its headers are the description)
      const extra = [];
      for (let i = 0; i < files.length; i++) {
        const s = m.webcodecsStream(files[i], new Uint8Array(await (await fetch(`/f/${i}`)).arrayBuffer()));
        if (s.config.codec === "vorbis") {
          extra.push(s.config);
          break;
        }
      }
      return m.support(extra);
    }, files.map((f) => basename(f))),
  ),
);
const rows = [];
for (let i = 0; i < files.length; i++) {
  const name = basename(files[i]);
  const r = await page.evaluate(
    async ({ i, name, rounds }) => {
      const m = await import("/page.js");
      const bytes = new Uint8Array(await (await fetch(`/f/${i}`)).arrayBuffer());
      const s = m.webcodecsStream(name, bytes);
      const out = { name, codec: s.config.codec, rate: s.config.sampleRate, ch: s.config.numberOfChannels, packets: s.packets.length };
      let wc = null;
      out.wc = [];
      for (let k = 0; k < rounds; k++) {
        wc = await m.decodeWebcodecs(s);
        out.wc.push([wc.wall, wc.cb]);
      }
      out.seconds = wc.pcm.length / s.config.numberOfChannels / s.config.sampleRate;
      if (s.heads) {
        const w = await WebAssembly.instantiate(await (await fetch("/dec.wasm")).arrayBuffer(), {});
        let nat = null;
        out.wasm = [];
        for (let k = 0; k < rounds; k++) {
          nat = m.decodeWasm(w.instance, s);
          out.wasm.push(nat.ms);
        }
        out.diff = m.compare(nat.pcm, wc.pcm);
      }
      out.lat = await m.latencyWebcodecs(s, 300);
      return out;
    },
    { i, name, rounds: ROUNDS },
  );
  // CPU of all processes over one more decode of each kind (decoders off the page thread count)
  const c0 = await cpu();
  await page.evaluate(async ({ i, name }) => {
    const m = await import("/page.js");
    const s = m.webcodecsStream(name, new Uint8Array(await (await fetch(`/f/${i}`)).arrayBuffer()));
    for (let k = 0; k < 3; k++) await m.decodeWebcodecs(s);
  }, { i, name });
  r.wcCpu = ((await cpu()) - c0) / 3;
  rows.push(r);
}
await browser.close();
server.close();

const med = (a) => [...a].sort((x, y) => x - y)[a.length >> 1];
console.log("\nfile                      codec   sec   wasm ms/s  WC wall ms/s  WC page ms/s  WC cpu ms/s  wait p50/p95 ms  max diff   SNR dB");
for (const r of rows) {
  const s = r.seconds;
  const wasm = r.wasm ? (med(r.wasm) / s).toFixed(2) : "-";
  const wall = (med(r.wc.map((x) => x[0])) / s).toFixed(2);
  const cb = (med(r.wc.map((x) => x[1])) / s).toFixed(2);
  const wcpu = ((r.wcCpu * 1000) / s).toFixed(2);
  const lat = `${r.lat.p50.toFixed(2)}/${r.lat.p95.toFixed(2)}`;
  const d = r.diff ? `${r.diff.max.toExponential(1)}  ${r.diff.snr.toFixed(1)}` : "-";
  console.log(`${r.name.padEnd(25)} ${r.codec.padEnd(7)} ${s.toFixed(1).padStart(5)} ${wasm.padStart(10)} ${wall.padStart(13)} ${cb.padStart(13)} ${wcpu.padStart(12)} ${lat.padStart(16)}  ${d}`);
}
console.log("\n" + JSON.stringify(rows));
