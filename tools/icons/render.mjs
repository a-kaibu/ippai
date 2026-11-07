// The app's icons (web/public/icons: favicon, PWA icons, apple-touch-icon): the 🍶 emoji of
// Noto Color Emoji (SIL OFL 1.1; noto-emoji-sake.woff2 is Google Fonts' COLRv1 subset of that one
// glyph, so the PNGs are sharp at any size), drawn by Chromium.
//
//   node tools/icons/render.mjs        (PLAYWRIGHT: path of the playwright module, as in tests/)
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "../../web/public/icons");
const font = readFileSync(join(here, "noto-emoji-sake.woff2")).toString("base64");
const BG = "#17181f"; // the app's background (manifest background_color)
// [file, size, emoji size / icon size, background]
const ICONS = [
  ["favicon-48.png", 48, 0.92, null],
  ["icon-192.png", 192, 0.9, null],
  ["icon-512.png", 512, 0.9, null],
  // (maskable: inside the 80% safe zone)
  ["maskable-512.png", 512, 0.58, BG],
  // (iOS fills a transparent icon with black)
  ["apple-touch-icon.png", 180, 0.7, BG],
];

const browser = await chromium.launch();
const page = await browser.newPage();
for (const [name, size, scale, bg] of ICONS) {
  await page.setViewportSize({ width: size, height: size });
  await page.setContent(
    `<style>@font-face{font-family:E;src:url(data:font/woff2;base64,${font})}</style>` +
      `<body style="margin:0;background:${bg ?? "transparent"};width:${size}px;height:${size}px;display:grid;place-items:center;overflow:hidden">` +
      `<span style="font-family:E;font-size:${Math.round((size * scale) / 1.17)}px;line-height:1">🍶</span></body>`,
  );
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({ path: join(out, name), omitBackground: !bg });
}
await browser.close();
