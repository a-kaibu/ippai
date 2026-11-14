// web/src/idec.ts imageDecode in real Chromium: the planes of tests/winref's jpeg_420 give the
// SHA-256 that gives Windows' bytes (decodesAsWindows), and the same JPEG with an Exif
// orientation that turns it (5-8: display sizes swapped) decodes all the same, in the sizes as
// coded (Windows' decoders do not turn it either); before, copyTo threw for those.
// (localhost: ImageDecoder is only in a secure context)
// node tests/idec-browser.mjs (PLAYWRIGHT, CHROME as in fetch-browser.mjs)
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

// (the sources as they are, made JavaScript by web's own TypeScript)
const ts = createRequire(new URL("../web/package.json", import.meta.url))("typescript");
const src = (f) =>
  ts.transpileModule(readFileSync(`web/src/${f}.ts`, "utf8"), {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
  }).outputText.replace(
    /from "\.\/([a-z-]+)"/g,
    'from "./$1.js"',
  );
const browser = await chromium.launch({ headless: true, executablePath: process.env.CHROME, chromiumSandbox: false });
try {
  const page = await browser.newPage();
  page.on("console", (m) => console.log("page:", m.text()));
  await page.route("http://localhost:9/**", (r) => {
    const f = new URL(r.request().url()).pathname.slice(1);
    if (f === "") return r.fulfill({ contentType: "text/html", body: "<!doctype html><title>idec</title>" });
    return r.fulfill({ contentType: "text/javascript", body: src(f.replace(/\.js$/, "")) });
  });
  await page.goto("http://localhost:9/");
  const got = await page.evaluate(async () => {
    const m = await import("/idec.js");
    const file = Uint8Array.from(atob(m.CHECK_JPEG), (c) => c.charCodeAt(0));
    // an APP1 Exif segment after SOI: big-endian TIFF, one IFD entry, Orientation (0x112) = o
    const withOrientation = (o) => {
      const tiff = [0x4d, 0x4d, 0, 42, 0, 0, 0, 8, 0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, o, 0, 0, 0, 0, 0, 0, 0, 0];
      const body = [0x45, 0x78, 0x69, 0x66, 0, 0, ...tiff];
      const seg = [0xff, 0xe1, (body.length + 2) >> 8, (body.length + 2) & 255, ...body];
      return Uint8Array.from([...file.subarray(0, 2), ...seg, ...file.subarray(2)]);
    };
    const hex = async (d) =>
      [...new Uint8Array(await crypto.subtle.digest("SHA-256", d))].map((b) => b.toString(16).padStart(2, "0")).join("");
    const out = { windows: await m.decodesAsWindows(m.imageDecode), turned: [] };
    for (const o of [1, 3, 5, 6, 7, 8]) {
      try {
        const r = await m.imageDecode(withOrientation(o));
        const d = new ImageDecoder({ data: withOrientation(o), type: "image/jpeg" });
        const { image } = await d.decode();
        const display = `${image.displayWidth}x${image.displayHeight}`;
        image.close();
        d.close();
        out.turned.push({ o, size: `${r.width}x${r.height}`, display, sha: await hex(r.data) });
      } catch (e) {
        out.turned.push({ o, error: String(e) });
      }
    }
    return out;
  });
  console.log(JSON.stringify(got));
  assert.equal(got.windows, true, "jpeg_420's planes give Windows' bytes");
  const sha = got.turned[0].sha;
  for (const t of got.turned) {
    assert.equal(t.error, undefined, `orientation ${t.o}: ${t.error}`);
    assert.equal(t.size, "21x13", `orientation ${t.o}: sizes as coded`);
    assert.equal(t.sha, sha, `orientation ${t.o}: the same planes`);
  }
  // (that the test does turn it: Chromium's display sizes are swapped for 5-8)
  assert.ok(got.turned.some((t) => t.display === "13x21"), "an orientation that swaps the display sizes");
  console.log("idec-browser: ok (planes as Windows; Exif orientations 1-8 decoded as coded)");
} finally {
  await browser.close();
}
