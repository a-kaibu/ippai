import { expect, test } from "vite-plus/test";
import { bytesOf, fileUrl, readSync, type SyncRequest } from "./sync-read";

/** What a browser's responseText is for `bytes` under "x-user-defined": 0x00-0x7F as they are, 0x80-0xFF as U+F700 + byte (U+F780-F7FF). */
const userDefined = (bytes: Uint8Array) =>
  String.fromCharCode(...Array.from(bytes, (b) => (b < 0x80 ? b : 0xf700 + b)));

function fake(files: Record<string, Uint8Array>, log: string[] = []): () => SyncRequest {
  return () => {
    let url = "";
    let mime = "";
    let status = 0;
    let text = "";
    return {
      open: (m, u, async) => (log.push(`${m} ${u} ${async}`), (url = u)),
      overrideMimeType: (m) => (mime = m),
      send: () => {
        const f = files[url];
        status = f ? 200 : 404;
        text = f && mime.includes("x-user-defined") ? userDefined(f) : f ? new TextDecoder().decode(f) : "";
      },
      get status() {
        return status;
      },
      get responseText() {
        return text;
      },
    };
  };
}

test("every byte value comes back as it was (property: random files)", () => {
  let seed = 12345;
  const rnd = () => ((seed = (seed * 1103515245 + 12345) >>> 0) >>> 8) / (1 << 24);
  for (let k = 0; k < 300; k++) {
    const n = Math.floor(rnd() * 600);
    const bytes = Uint8Array.from({ length: n }, () => Math.floor(rnd() * 256));
    if (k === 0) bytes.set(Uint8Array.from({ length: 256 }, (_, i) => i).subarray(0, n));
    expect(bytesOf(userDefined(bytes))).toEqual(bytes);
    expect(readSync("u", fake({ u: bytes }))).toEqual(bytes);
  }
});

test("a synchronous GET of the file's URL; a missing file or a failed request is null", () => {
  const log: string[] = [];
  const data = new TextEncoder().encode("セーブ");
  const url = fileUrl("http://127.0.0.1:9/tree/g/game/", "www/data/日本 語.json");
  expect(url).toBe("http://127.0.0.1:9/tree/g/game/www/data/%E6%97%A5%E6%9C%AC%20%E8%AA%9E.json");
  expect(readSync(url, fake({ [url]: data }, log))).toEqual(data);
  expect(log).toEqual([`GET ${url} false`]);
  expect(readSync(url + "x", fake({ [url]: data }))).toBeNull();
  const throwing = (): SyncRequest => ({
    open: () => undefined,
    overrideMimeType: () => undefined,
    send: () => {
      throw new Error("NetworkError");
    },
    status: 0,
    responseText: "",
  });
  expect(readSync(url, throwing)).toBeNull();
});
