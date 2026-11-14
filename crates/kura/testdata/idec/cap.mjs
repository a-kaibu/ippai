// node cap.mjs OUT.json FILE.jpg...: Chrome's ImageDecoder planes of each, laid out as web/src/idec.ts copies them (make.sh)
import { createRequire } from 'module'; const require = createRequire(import.meta.url); const { chromium } = require(process.env.PLAYWRIGHT ?? 'playwright');
import fs from 'fs';
const b = await chromium.launch();
const p = await b.newPage();
await p.goto(process.env.URL);
const out = {};
for (const f of process.argv.slice(3)) {
  const data = [...fs.readFileSync(f)];
  out[f] = await p.evaluate(async (data) => {
    const d = new ImageDecoder({ data: new Uint8Array(data), type: 'image/jpeg', colorSpaceConversion: 'none' });
    const { image } = await d.decode();
    const w = image.displayWidth, h = image.displayHeight;
    const cw = image.format === 'I444' ? w : (w + 1) >> 1, ch = image.format === 'I420' ? (h + 1) >> 1 : h;
    const lay = image.format.startsWith('I') ? [{ offset: 0, stride: w }, { offset: w * h, stride: cw }, { offset: w * h + cw * ch, stride: cw }] : [{ offset: 0, stride: w * 4 }];
    const buf = new Uint8Array(image.format.startsWith('I') ? w * h + 2 * cw * ch : w * h * 4);
    await image.copyTo(buf, { layout: lay });
    const r = { fmt: image.format, w, h, cs: image.colorSpace.toJSON(), b64: btoa(String.fromCharCode(...buf)) };
    image.close();
    return r;
  }, data);
}
fs.writeFileSync(process.argv[2], JSON.stringify(out));
await b.close();
