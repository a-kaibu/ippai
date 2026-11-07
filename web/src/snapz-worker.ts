// The save states' zstd (savestate.ts), off the page's thread: packs the blocks of a state from
// the program's shared memory, and unpacks a state into a new one. zstd is Facebook's, built by
// crates/sake/build.sh (crates/sake/snapz/snapz.c → public/sake/snapz.mjs).

interface Snapz {
  HEAPU8: Uint8Array;
  _malloc(n: number): number;
  _snapz_bound(n: number): number;
  _snapz_compress(dst: number, cap: number, src: number, n: number, level: number): number;
  _snapz_decompress(dst: number, cap: number, src: number, n: number): number;
}

export type Request =
  | { op: "pack"; url: string; memory: SharedArrayBuffer; ranges: number[]; block: number }
  | { op: "unpack"; url: string; memory: SharedArrayBuffer; file: ArrayBuffer; at: number };
export type Reply =
  | { ok: true; parts: { a: number; n: number; frame: Uint8Array }[]; raw: number }
  | { ok: true; raw: number }
  | { ok: false; error: string };

let snapz: Promise<Snapz> | undefined;
/** The module's own memory: a block in, its frame out */
let io: { src: number; dst: number; cap: number; size: number } | undefined;

function buffers(z: Snapz, size: number) {
  if (!io || io.size < size) {
    const cap = z._snapz_bound(size);
    io = { src: z._malloc(size) >>> 0, dst: z._malloc(cap) >>> 0, cap, size };
  }
  return io;
}

function zero(mem: Uint8Array, at: number, n: number): boolean {
  const w = new BigUint64Array(mem.buffer, at, n >>> 3);
  for (let i = 0; i < w.length; i++) if (w[i] !== 0n) return false;
  return true;
}

async function handle(r: Request): Promise<Reply> {
  snapz ??= import(/* @vite-ignore */ r.url).then((m: { default: () => Promise<Snapz> }) => m.default());
  const z = await snapz;
  const mem = new Uint8Array(r.memory);
  if (r.op === "pack") {
    const parts = [];
    let raw = 0;
    const b = buffers(z, r.block);
    for (let k = 0; k < r.ranges.length; k += 2) {
      const end = r.ranges[k]! + r.ranges[k + 1]!;
      for (let a = r.ranges[k]!; a < end; a += r.block) {
        const n = Math.min(r.block, end - a);
        // (a new memory is all zeros: blocks of zeros are left out)
        if (zero(mem, a, n)) continue;
        z.HEAPU8.set(mem.subarray(a, a + n), b.src);
        const len = z._snapz_compress(b.dst, b.cap, b.src, n, 1) >>> 0;
        if (!len) throw new Error(`zstd: block at ${a.toString(16)}`);
        parts.push({ a, n, frame: z.HEAPU8.slice(b.dst, b.dst + len) });
        raw += n;
      }
    }
    return { ok: true, parts, raw };
  }
  const f = new DataView(r.file);
  const bytes = new Uint8Array(r.file);
  let at = r.at,
    raw = 0;
  for (;;) {
    const a = f.getUint32(at, true);
    if (a === 0xffffffff) break;
    const n = f.getUint32(at + 4, true),
      c = f.getUint32(at + 8, true);
    const b = buffers(z, Math.max(n, c));
    z.HEAPU8.set(bytes.subarray(at + 12, at + 12 + c), b.src);
    const got = z._snapz_decompress(b.dst, b.cap, b.src, c) >>> 0;
    if (got !== n) throw new Error(`block at ${a.toString(16)}: ${got} bytes, not ${n}`);
    mem.set(z.HEAPU8.subarray(b.dst, b.dst + n), a);
    raw += n;
    at += 12 + c;
  }
  return { ok: true, raw };
}

self.onmessage = (e: MessageEvent<Request>) => {
  handle(e.data).then(
    (r) => {
      const t = "parts" in r ? r.parts.map((p) => p.frame.buffer as ArrayBuffer) : [];
      self.postMessage(r, { transfer: t });
    },
    (err: unknown) =>
      self.postMessage({
        ok: false,
        error: err instanceof Error ? err.message : String(err),
      } satisfies Reply),
  );
};
