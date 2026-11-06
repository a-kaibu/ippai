import { expect, test } from "vite-plus/test";
import { SaveTransport } from "./save-transport";

test("a lost acknowledgement can be recovered only for identical saved bytes", async () => {
  let written = false;
  const transport = new SaveTransport(async (_url, init) => {
    if (init?.method === "PUT") {
      written = true;
      return new Response(null, { status: 412 });
    }
    return written
      ? new Response(new Uint8Array([7]), { headers: { etag: '"1"' } })
      : new Response(null, { status: 404 });
  });
  await transport.load("/save");
  await transport.store("/save", new Uint8Array([7]));
  await expect(transport.store("/save", new Uint8Array([8]))).rejects.toThrow("別の端末");
});

test("writes await the previous acknowledgement and carry its revision", async () => {
  const revisions: string[] = [];
  let release!: () => void;
  const gate = new Promise<void>((r) => {
    release = r;
  });
  const request: typeof fetch = async (_url, init) => {
    if (!init?.method) return new Response(null, { status: 404 });
    revisions.push(new Headers(init.headers).get("if-match")!);
    if (revisions.length === 1) await gate;
    return new Response(null, { status: 204, headers: { etag: `"${revisions.length}"` } });
  };
  const transport = new SaveTransport(request);
  await transport.load("/save");
  const a = transport.store("/save", new Uint8Array([1]));
  const b = transport.store("/save", new Uint8Array([2]));
  await new Promise((r) => setTimeout(r, 0));
  expect(revisions).toEqual(['"0"']);
  release();
  await Promise.all([a, b]);
  expect(revisions).toEqual(['"0"', '"1"']);
});

test("failed writes do not advance the revision", async () => {
  const revisions: string[] = [];
  const transport = new SaveTransport(async (_url, init) => {
    if (!init?.method) return new Response(null, { status: 404 });
    revisions.push(new Headers(init.headers).get("if-match")!);
    return new Response(null, { status: revisions.length === 1 ? 503 : 204, headers: { etag: '"1"' } });
  });
  await transport.load("/save");
  await expect(transport.store("/save", new Uint8Array([1]))).rejects.toThrow();
  await transport.store("/save", new Uint8Array([2]));
  expect(revisions).toEqual(['"0"', '"0"']);
});

test("a game started offline syncs later only over the same server bytes", async () => {
  let server: Uint8Array | null = new Uint8Array([5]);
  const request: typeof fetch = async (_url, init) => {
    if (init?.method === "PUT") {
      server = new Uint8Array(init.body as Uint8Array);
      return new Response(null, { status: 204, headers: { etag: '"2"' } });
    }
    return server
      ? new Response(server.slice(), { headers: { etag: '"1"' } })
      : new Response(null, { status: 404 });
  };
  const same = new SaveTransport(request);
  same.startedOffline("/save", new Uint8Array([5]));
  await same.store("/save", new Uint8Array([6]));
  expect(Array.from(server!)).toEqual([6]);

  // another device saved meanwhile: kept, the write is a conflict
  const other = new SaveTransport(request);
  other.startedOffline("/save", new Uint8Array([5]));
  await expect(other.store("/save", new Uint8Array([7]))).rejects.toThrow("別の端末");
  expect(Array.from(server!)).toEqual([6]);

  server = null;
  const fresh = new SaveTransport(request);
  fresh.startedOffline("/save", null);
  await fresh.store("/save", new Uint8Array([8]));
  expect(Array.from(server!)).toEqual([8]);
});
