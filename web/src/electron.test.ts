import { expect, test } from "vite-plus/test";
import { electronNode } from "./electron";

const window = (prefs: string) => `mainWindow = new BrowserWindow({
  "width": 1280, "height": 720,
  webPreferences: { ${prefs} }
});
mainWindow.loadURL('file://' + __dirname + '/index.html');`;

test("electronNode: TyranoScript v5's main.js (Electron 7, nodeIntegration: true)", () => {
  expect(electronNode(window("nodeIntegration: true"), "7.1.2")).toBe(true);
  expect(electronNode(window('"nodeIntegration": true'), "v9.4.0")).toBe(true);
});

test("electronNode: isolated or without Node integration, the page has no Node", () => {
  expect(electronNode(window("nodeIntegration: true"), "13.6.9")).toBe(false);
  expect(electronNode(window("nodeIntegration: true, contextIsolation: true"), "7.1.2")).toBe(false);
  expect(electronNode(window("nodeIntegration: true, contextIsolation: false"), "22.0.0")).toBe(true);
  expect(electronNode(window("nodeIntegration: false"), "7.1.2")).toBe(false);
  expect(electronNode(window("preload: path.join(__dirname, 'preload.js')"), "7.1.2")).toBe(false);
  expect(electronNode(window("// nodeIntegration: true"), "7.1.2")).toBe(false);
  expect(electronNode(window("/* nodeIntegration: true */"), "7.1.2")).toBe(false);
  // (no version file: Electron 12 and later are the likelier)
  expect(electronNode(window("nodeIntegration: true"), null)).toBe(false);
  expect(electronNode(window("nodeIntegration: true"), "garbage")).toBe(false);
});

test("electronNode: a URL's // is not a comment", () => {
  expect(electronNode(`const u = "http://example.com"; ${window("nodeIntegration: true")}`, "7.1.2")).toBe(
    true,
  );
});

// property: for any options in any order, Node exactly when integration is on and the page is
// not isolated (explicitly, or by the version's default)
test("electronNode: every combination of options and versions", () => {
  let seed = 12345;
  const next = (n: number) => ((seed = (seed * 1103515245 + 12345) >>> 0) >>> 8) % n;
  for (let i = 0; i < 2000; i++) {
    const integration = [true, false, null][next(3)];
    const isolation = [true, false, null][next(3)];
    const major = next(30) + 1;
    const opts = [
      integration === null ? null : `nodeIntegration: ${integration}`,
      isolation === null ? null : `${next(2) ? '"contextIsolation"' : "contextIsolation"}: ${isolation}`,
      next(2) ? "webSecurity: false" : null,
      next(2) ? "enableRemoteModule: true" : null,
    ].filter((o): o is string => o !== null);
    for (let j = opts.length - 1; j > 0; j--) {
      const k = next(j + 1);
      [opts[j], opts[k]] = [opts[k]!, opts[j]!];
    }
    const version = next(5) ? `${major}.${next(10)}.${next(10)}` : null;
    const isolated = isolation ?? (version === null || major >= 12);
    expect(electronNode(window(opts.join(", ")), version), `${opts.join(", ")} ${String(version)}`).toBe(
      integration === true && !isolated,
    );
  }
});
