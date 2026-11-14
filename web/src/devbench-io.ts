// The device benchmark's storage step (web/src/bench.ts): how fast the phone reads what a saved
// game keeps on it. OPFS through a synchronous access handle in a worker (as the network worker
// reads chunks, web/src/chunkstore.ts) and Cache Storage (the older store). Everything it writes
// is removed again.

const MB = 1 << 20;
export const FILE_MB = 64;
const READ = 256 << 10;
const READS = 256;

// (a worker of its own: createSyncAccessHandle exists only in dedicated workers)
const WORKER = `
onmessage = async ({ data: [fileMb, read, reads] }) => {
  const out = {};
  const dir = await navigator.storage.getDirectory();
  const name = "ippai-bench.tmp";
  try {
    const file = await dir.getFileHandle(name, { create: true });
    const h = await file.createSyncAccessHandle();
    const piece = new Uint8Array(${MB});
    for (let i = 0; i < piece.length; i++) piece[i] = (i * 2654435761) >>> 24;
    let t = performance.now();
    for (let i = 0; i < fileMb; i++) h.write(piece, { at: i * ${MB} });
    h.flush();
    out["opfs_write.mb_s"] = fileMb / ((performance.now() - t) / 1000);
    const buf = new Uint8Array(read);
    let x = 12345;
    t = performance.now();
    for (let i = 0; i < reads; i++) {
      x = (x * 1103515245 + 12345) >>> 0;
      h.read(buf, { at: (x % (fileMb * ${MB} - read)) & ~4095 });
    }
    const ms = performance.now() - t;
    out["opfs_read.ms"] = ms / reads;
    out["opfs_read.mb_s"] = (read * reads) / ${MB} / (ms / 1000);
    h.close();
  } finally {
    await dir.removeEntry(name).catch(() => {});
  }
  postMessage(out);
};`;

export async function storageBench(metric: (name: string, v: number) => void): Promise<void> {
  const url = URL.createObjectURL(new Blob([WORKER], { type: "text/javascript" }));
  const w = new Worker(url);
  try {
    const out = await new Promise<Record<string, number>>((done, fail) => {
      w.onmessage = (e) => done(e.data as Record<string, number>);
      w.onerror = (e) => fail(new Error(e.message || "OPFS の計測に失敗しました"));
      w.postMessage([FILE_MB, READ, READS]);
    });
    for (const [k, v] of Object.entries(out)) metric(k, v);
  } finally {
    w.terminate();
    URL.revokeObjectURL(url);
  }
  if (typeof caches === "undefined") return;
  const name = "ippai-bench-tmp";
  const cache = await caches.open(name);
  try {
    const body = new Uint8Array(4 * MB);
    const n = 8;
    let t = performance.now();
    for (let i = 0; i < n; i++) await cache.put(`/bench-tmp/${i}`, new Response(body));
    metric("cache_write.mb_s", (n * 4) / ((performance.now() - t) / 1000));
    t = performance.now();
    for (let i = 0; i < n; i++) await (await cache.match(`/bench-tmp/${i}`))!.arrayBuffer();
    metric("cache_read.mb_s", (n * 4) / ((performance.now() - t) / 1000));
  } finally {
    await caches.delete(name);
  }
}
