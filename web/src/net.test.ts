import { expect, test } from "vite-plus/test";
import { fetchRetry, HttpError, treeChunks, watchRetries } from "./net";

const bytes = (n: number) => Uint8Array.from({ length: n }, (_, i) => i % 251);

test("dropped connections and 503s are retried; a 404 is final", async () => {
  let n = 0;
  const request: typeof fetch = async () => {
    n++;
    if (n === 1) throw new TypeError("Failed to fetch");
    if (n === 2) return new Response(null, { status: 503 });
    return new Response(bytes(1000));
  };
  expect(Array.from(await fetchRetry("/x", {}, { request }))).toEqual(Array.from(bytes(1000)));
  expect(n).toBe(3);
  let m = 0;
  const missing: typeof fetch = async () => (m++, new Response(null, { status: 404 }));
  await expect(fetchRetry("/x", {}, { request: missing })).rejects.toBeInstanceOf(HttpError);
  expect(m).toBe(1);
});

test("a body that stops arriving is aborted and fetched again", async () => {
  let n = 0;
  const request: typeof fetch = async (_u, init) => {
    n++;
    if (n > 1) return new Response(bytes(70000));
    // half the body, then nothing: the stall watch aborts it
    const body = new ReadableStream<Uint8Array>({
      start(c) {
        c.enqueue(bytes(100));
        init!.signal!.addEventListener("abort", () => c.error(init!.signal!.reason));
      },
    });
    return new Response(body);
  };
  const t = Date.now();
  expect((await fetchRetry("/x", {}, { request, stallMs: 150 })).length).toBe(70000);
  expect(n).toBe(2);
  expect(Date.now() - t).toBeLessThan(3000);
});

test("gives up after giveUpMs, and the page's retries are counted while they last", async () => {
  const seen: number[] = [];
  const stop = watchRetries((r) => seen.push(r));
  const request: typeof fetch = async () => {
    throw new TypeError("Failed to fetch");
  };
  await expect(fetchRetry("/x", {}, { request, giveUpMs: 500 })).rejects.toThrow("Failed to fetch");
  stop();
  expect(seen[0]).toBe(1);
  expect(seen.at(-1)).toBe(0);
});

test("a tree's chunk hashes (holes left out)", () => {
  const h = (c: string) => c.repeat(32);
  const tsv = `vnvm-tree\t1\t262144\nd\t755\t0\tdir\nf\t644\t0\tdir/a\t300000\t${h("a")},,${h("b")}\nf\t644\t0\tempty\t0\t\nl\t777\t0\tlink\ta\n`;
  expect(treeChunks(tsv)).toEqual([h("a"), h("b")]);
});
