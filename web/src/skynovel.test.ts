import { expect, test } from "vite-plus/test";
import { FILE } from "./frame-protocol";
import { toApp } from "./skynovel";

const PAGE = "http://127.0.0.1:8657/tree/g/x/resources/app/doc/app/index.htm";

function app(items = new Map<string, string>()) {
  const changes: string[] = [];
  return { items, changes, to: toApp(items, (k) => changes.push(k), PAGE) };
}

test("toApp: the app path is the frame's resources/app, where SKYNovel finds doc/crypto_prj/", async () => {
  const { to } = app();
  const info = await to.getInfo();
  expect(info.getAppPath).toBe("http://127.0.0.1:8657/tree/g/x/resources/app");
  expect(info.isPackaged).toBe(true);
  expect(info.platform).toBe("win32");
  // (SysApp.loaded: cur = getAppPath + "/doc/" + "crypto_prj/")
  expect(new URL(`${info.getAppPath}/doc/crypto_prj/path.json`).pathname).toBe(
    "/tree/g/x/resources/app/doc/crypto_prj/path.json",
  );
  expect((await toApp(new Map(), () => {}, "http://h/tree/g/a/b.html").getInfo()).getAppPath).toBe(
    "http://h/tree/g/a",
  );
});

test("toApp: the electron-store data is empty on the first boot and kept with the saves after", async () => {
  const { items, changes, to } = app();
  await to.Store({ name: "data_" });
  expect(await to.Store_isEmpty()).toBe(true);
  const data = { sys: { "const.sn.nativeWindow.x": 3 }, mark: { 0: { a: "ページ" } }, kidoku: {} };
  await to.flush(data);
  expect(changes).toEqual([`${FILE}skynovel/store/data_`]);
  // (a later boot: the saves come back as items)
  const again = app(new Map(items)).to;
  await again.Store({ name: "data_" });
  expect(await again.Store_isEmpty()).toBe(false);
  expect(await again.Store_get()).toEqual(data);
  // (another store name is another store)
  await again.Store({ name: "data" });
  expect(await again.Store_isEmpty()).toBe(true);
});

test("toApp: files written under userData are kept, copied and removed as folders", async () => {
  const { items, to } = app();
  const ud = (await to.getInfo()).userData;
  await to.writeFileSync(`${ud}/storage/1/pic.png`, new Uint8Array([0, 255, 128]));
  await to.writeFileSync(`${ud}/storage/1/a.txt`, "あ");
  await to.appendFile(`${ud}/storage/1/a.txt`, "い");
  expect(await to.readFileSync(`${ud}/storage/1/a.txt`)).toBe("あい");
  expect(Array.from(await to.readFile(`${ud}/storage/1/pic.png`))).toEqual([0, 255, 128]);
  expect(await to.existsSync(`${ud}/storage/1/`)).toBe(true);
  expect(await to.existsSync(`${ud}/storage/2/`)).toBe(false);
  await to.copySync(`${ud}/storage/1/`, `${ud}/storage/2/`);
  expect(await to.readFileSync(`${ud}\\storage\\2\\a.txt`)).toBe("あい");
  await to.removeSync(`${ud}/storage/1/`);
  expect(await to.existsSync(`${ud}/storage/1/`)).toBe(false);
  expect(await to.existsSync(`${ud}/storage/2/a.txt`)).toBe(true);
  // (no key outside the files' prefix)
  for (const k of items.keys()) expect(k.startsWith(`${FILE}skynovel/`)).toBe(true);
});

test("toApp: message boxes answer Cancel (update_check downloads nothing), window calls do nothing", async () => {
  const { to } = app();
  expect(await to.showMessageBox({ cancelId: 1 })).toEqual({ response: 1, checkboxChecked: false });
  expect(await to.isSimpleFullScreen()).toBe(false);
  await to.window(true, 0, 0, 1024, 768);
  to.on("fire", () => {});
});

// property: any run of writes, copies and removes of files in a few folders matches a plain
// map of paths to contents
test("toApp: writes, copies and removes match a model", async () => {
  let seed = 4242;
  const next = (n: number) => ((seed = (seed * 1103515245 + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 200; run++) {
    const { to } = app();
    const model = new Map<string, string>();
    const dirs = ["s/1", "s/2", "s/1/x"];
    const file = () => `${dirs[next(3)]}/${"abc"[next(3)]}`;
    for (let i = 0; i < 20; i++) {
      const op = next(4);
      if (op === 0) {
        const [p, v] = [file(), String(next(1000))];
        await to.writeFileSync(p, v);
        model.set(p, v);
      } else if (op === 1) {
        const [a, b] = [dirs[next(3)]!, dirs[next(3)]!];
        if (b.startsWith(`${a}/`) || a === b) continue;
        await to.copySync(`${a}/`, `${b}/`);
        for (const [k, v] of Array.from(model))
          if (k.startsWith(`${a}/`)) model.set(b + k.slice(a.length), v);
      } else if (op === 2) {
        const d = dirs[next(3)]!;
        await to.removeSync(`${d}/`);
        for (const k of Array.from(model.keys())) if (k.startsWith(`${d}/`)) model.delete(k);
      } else {
        const p = file();
        expect(await to.readFileSync(p)).toBe(model.get(p) ?? "");
        expect(await to.existsSync(p)).toBe(model.has(p));
      }
    }
  }
});
