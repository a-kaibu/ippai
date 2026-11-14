// Pictures the browser decodes for the guest (crates/kura/src/idec.rs): JPEG files sake's WIC,
// GDI+, D3DX and OLE would decode with jpeg-decoder in wasm. ImageDecoder (libjpeg-turbo's islow
// IDCT, in native SIMD code) hands over the Y, Cb and Cr planes, copied into kura's room as they
// came (kura::idec::Layout); kura upsamples them and turns them into RGB as libjpeg (and Windows)
// does. At the start a small JPEG is decoded and its planes set against those that give Windows'
// answer: a browser that decodes it otherwise is never listed to (the guest then decodes every
// picture itself).

import { memGate, type Gate } from "./memgate";

export interface IdecWasm {
  wasmMemory: WebAssembly.Memory;
  _malloc(size: number): number;
  _vnhost_idec_doorbell(): number;
  _vnhost_idec_list(out: number, max: number): number;
  _vnhost_idec_begin(id: number, w: number, h: number, layout: number): number;
  _vnhost_idec_done(id: number, ok: number): void;
}

/** kura::idec::Layout: the planes' numbers (VideoFrame formats). */
export const LAYOUTS: Record<string, number> = { I420: 1, I422: 2, I444: 3, BGRX: 4, RGBX: 5 };

/** A JPEG file -> its planes, laid out as kura::idec::Layout `layout` says. */
export type Decode = (
  file: Uint8Array,
) => Promise<{ width: number; height: number; layout: number; data: Uint8Array }>;

/** The planes' (offset, stride) as kura::idec::Layout packs them: rows one after the other. */
export function planes(format: string, w: number, h: number): { offset: number; stride: number }[] {
  if (format === "BGRX" || format === "RGBX") return [{ offset: 0, stride: w * 4 }];
  const cw = format === "I444" ? w : (w + 1) >> 1;
  const ch = format === "I420" ? (h + 1) >> 1 : h;
  return [
    { offset: 0, stride: w },
    { offset: w * h, stride: cw },
    { offset: w * h + cw * ch, stride: cw },
  ];
}

/** The bytes of `planes`' layout. */
export function planesSize(format: string, w: number, h: number): number {
  const p = planes(format, w, h);
  if (p.length === 1) return w * h * 4;
  return p[2]!.offset + (p[2]!.offset - p[1]!.offset);
}

type WaitAsync = (
  a: Int32Array,
  i: number,
  v: number,
  t: number,
) => { async: boolean; value: Promise<string> | string };

const MAX = 16;

/** ImageDecoder's planes, as they came (no color conversion: Exif and ICC are kura's to drop). */
export const imageDecode: Decode = async (file) => {
  const d = new ImageDecoder({ data: file, type: "image/jpeg", colorSpaceConversion: "none" });
  try {
    const { image } = await d.decode();
    try {
      const f = image.format ?? "";
      const layout = LAYOUTS[f];
      if (!layout) throw new Error(`planes ${f}`);
      const [w, h] = [image.displayWidth, image.displayHeight];
      const data = new Uint8Array(planesSize(f, w, h));
      await image.copyTo(data, { layout: planes(f, w, h) });
      return { width: w, height: h, layout, data };
    } finally {
      image.close();
    }
  } finally {
    d.close();
  }
};

/** tests/winref imaging's jpeg_420 (21x13, 4:2:0), and the SHA-256 of its I420 planes that give
 * Windows' RGB of it (tests/winref/golden/imaging-32/jpeg_420.png; crates/kura/testdata/idec). */
export const CHECK_JPEG =
  "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAMCAgMCAgMDAwMEAwMEBQgFBQQEBQoHBwYIDAoMDAsKCwsNDhIQDQ4RDgsLEBYQERMUFRUVDA8XGBYUGBIUFRT/2wBDAQMEBAUEBQkFBQkUDQsNFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBT/wAARCAANABUDASIAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwD5s8NfAuBbXyoombS2IMs5X51bjgcey9j1r1Pw/wDA3d9l8232eXj7Dgf63pjd+S+nU19PeGvAOlS2v25YVjt4iEazCjY545PbuO3avVPD/wAO9Nh+y7h5n2zHk5X/AI9s4xt+m4dMfdFehmXHTdLmi1ycsWrJ8vI52g0n7ypuppTg/fhV9+o3T0PzbgXxIr3hq7/1f5236W0Wp8p2XwJspkZ9QV7e8ON8ca5Uccdj2x3or7xsvBOj6aht57RL2VMZnkUbmyM85B6Zx17UV+SY3xCyyniakMXOgqqk1L2lKvKpzX97nlBqEp3vzSguVu7jpY/s3B+I+KeHg4uVrdGrfI//2Q==";
export const CHECK_SHA256 = "857e5bba511275d3e3f85109c800fc7b87733e428bd4eeb9de01bfc3f193b7ef";

/** Whether `decode` gives the planes of CHECK_JPEG that give Windows' bytes. */
export async function decodesAsWindows(decode: Decode): Promise<boolean> {
  try {
    const file = Uint8Array.from(atob(CHECK_JPEG), (c) => c.charCodeAt(0));
    const r = await decode(file);
    if (r.width !== 21 || r.height !== 13 || r.layout !== LAYOUTS.I420) return false;
    const h = new Uint8Array(await crypto.subtle.digest("SHA-256", new Uint8Array(r.data)));
    return [...h].map((b) => b.toString(16).padStart(2, "0")).join("") === CHECK_SHA256;
  } catch {
    return false;
  }
}

export class ImageDecode {
  private io: number;
  private doorbell: number;
  private tickKey = {};
  /** pictures decoded, and ms spent (in the browser's decoder and the copy) */
  count = 0;
  ms = 0;

  constructor(
    private wasm: IdecWasm,
    private note: (line: string) => void,
    private decode: Decode = imageDecode,
    private gate: Gate = memGate,
    check = true,
    watch = true,
  ) {
    this.io = wasm._malloc(4 * 3 * MAX) >>> 0;
    this.doorbell = wasm._vnhost_idec_doorbell() >>> 0;
    void this.start(check, watch);
  }

  private async start(check: boolean, watch: boolean): Promise<void> {
    if (check && !(await decodesAsWindows(this.decode))) {
      this.note("idec: this browser's JPEG decoder does not give Windows' pixels: pictures decode in wasm");
      return;
    }
    // (the first list says there is a page)
    this.tick();
    if (watch) void this.watch();
  }

  private async watch(): Promise<void> {
    const i32 = () => new Int32Array(this.wasm.wasmMemory.buffer);
    let seen = Atomics.load(i32(), this.doorbell >>> 2);
    for (;;) {
      const w = (Atomics as unknown as { waitAsync: WaitAsync }).waitAsync(
        i32(),
        this.doorbell >>> 2,
        seen,
        1000,
      );
      if (w.async) await w.value;
      seen = Atomics.load(i32(), this.doorbell >>> 2);
      this.tick();
    }
  }

  /** Files waiting: each copied out and decoded (on each doorbell). */
  tick(): void {
    // (a save state is being taken: listed once it is done, as the guest waits for an answer)
    if (this.gate.closed) return this.gate.run(() => this.tick(), this.tickKey);
    const n = this.wasm._vnhost_idec_list(this.io, MAX);
    const u32 = new Uint32Array(this.wasm.wasmMemory.buffer, this.io, n * 3);
    for (let k = 0; k < n; k++) {
      const [id, ptr, len] = [u32[k * 3]!, u32[k * 3 + 1]!, u32[k * 3 + 2]!];
      void this.one(id, new Uint8Array(this.wasm.wasmMemory.buffer, ptr, len).slice());
    }
  }

  private async one(id: number, file: Uint8Array): Promise<void> {
    const t = performance.now();
    let r: Awaited<ReturnType<Decode>> | null = null;
    try {
      r = await this.decode(file);
    } catch (e) {
      this.note(`idec ${id}: ${String(e)}`);
    }
    this.gate.run(() => {
      let ok = 0;
      if (r) {
        const p = this.wasm._vnhost_idec_begin(id, r.width, r.height, r.layout) >>> 0;
        if (p) {
          new Uint8Array(this.wasm.wasmMemory.buffer, p, r.data.length).set(r.data);
          ok = 1;
        }
      }
      this.wasm._vnhost_idec_done(id, ok);
      this.count++;
      this.ms += performance.now() - t;
    });
  }
}
