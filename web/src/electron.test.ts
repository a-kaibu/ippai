import { expect, test } from "vite-plus/test";
import { electronNode, electronPages } from "./electron";

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

test("electronPages: SKYNovel's main script hands its page to initRenderer (あにまるめいど!, #234)", () => {
  const code = `guiWin = require('@famibee/skynovel/appMain').initRenderer(
		path.join(__dirname, 'app/index.htm'),
		pkg.version, { width: 1024 });
	icon_path: path.join(__dirname, 'app/icon.png'),`;
  expect(electronPages("doc/app.js", code)).toEqual(["doc/app/index.htm", "app/index.htm"]);
});

test("electronPages: loadURL, loadFile and file:// names", () => {
  expect(electronPages("main.js", "win.loadURL('file://' + __dirname + '/index.html')")).toEqual([
    "index.html",
  ]);
  expect(electronPages("src/main.js", 'win.loadFile("www/index.html")')).toEqual([
    "src/www/index.html",
    "www/index.html",
  ]);
  expect(electronPages("main.js", "win.loadURL(`file://${__dirname}/game/index.html`)")).toEqual([
    "game/index.html",
  ]);
  expect(electronPages("a/b/main.js", "load('../page.html')")).toEqual(["a/page.html"]);
});

test("electronPages: web URLs and names that leave the folder are not pages", () => {
  expect(electronPages("main.js", "open('https://example.com/index.html'); load('../../x.html')")).toEqual(
    [],
  );
  expect(electronPages("main.js", "const s = 'not a page'; const t = \"x.js\"")).toEqual([]);
});

// property: every answer is a path in the app folder (no "..", no empty parts, no scheme) that
// ends in .htm(l), and one named in the script beside it comes first
test("electronPages: answers stay in the app folder", () => {
  let seed = 777;
  const next = (n: number) => ((seed = (seed * 1103515245 + 12345) >>> 0) >>> 8) % n;
  const parts = ["a", "..", ".", "app", "ページ", "x y", ""];
  for (let i = 0; i < 3000; i++) {
    const main = [...Array(next(3))]
      .map(() => parts[next(4) + 2])
      .filter(Boolean)
      .concat("main.js")
      .join("/");
    const names = [...Array(next(4))].map(
      () =>
        [...Array(1 + next(4))].map(() => parts[next(parts.length)]).join("/") +
        [".html", ".htm", ".js"][next(3)],
    );
    const code = names
      .map((n) => `load(${["'", '"', "`"][next(3)]}${n}${["'", '"', "`"][next(3)]})`)
      .join(";");
    for (const p of electronPages(main, code)) {
      expect(p).toMatch(/\.html?$/i);
      expect(p.split("/")).not.toContain("..");
      expect(p.split("/")).not.toContain("");
      expect(p.split("/")).not.toContain(".");
    }
  }
});
