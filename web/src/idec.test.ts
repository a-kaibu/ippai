import { expect, test } from "vite-plus/test";
import { Gate } from "./memgate";
import {
  CHECK_JPEG,
  decodesAsWindows,
  ImageDecode,
  LAYOUTS,
  planes,
  planesSize,
  type Decode,
  type IdecWasm,
} from "./idec";

const tick = (ms = 0) => new Promise((r) => setTimeout(r, ms));

/** kura's side of crates/kura/src/idec.rs, in memory: files waiting, what came back. */
class FakeKura implements IdecWasm {
  wasmMemory = new WebAssembly.Memory({ initial: 4 });
  private top = 1024;
  files = new Map<number, { data: number[]; listed: boolean; px: number; w: number; h: number }>();
  done: [number, number][] = [];
  lists = 0;
  _malloc(n: number): number {
    const p = this.top;
    this.top += (n + 15) & ~15;
    return p;
  }
  _vnhost_idec_doorbell(): number {
    return 512;
  }
  _vnhost_idec_list(out: number, max: number): number {
    this.lists++;
    const u32 = new Uint32Array(this.wasmMemory.buffer);
    let k = 0;
    for (const [id, f] of this.files) {
      if (k === max || f.listed) continue;
      f.listed = true;
      const p = this._malloc(f.data.length);
      new Uint8Array(this.wasmMemory.buffer).set(f.data, p);
      u32.set([id, p, f.data.length], out / 4 + k * 3);
      k++;
    }
    return k;
  }
  _vnhost_idec_begin(id: number, w: number, h: number, layout: number): number {
    const f = this.files.get(id);
    if (!f || layout !== LAYOUTS.BGRX) return 0;
    f.px = this._malloc(w * h * 4);
    f.w = w;
    f.h = h;
    return f.px;
  }
  _vnhost_idec_done(id: number, ok: number): void {
    this.done.push([id, ok]);
  }
  pixels(id: number): number[] {
    const f = this.files.get(id)!;
    return [...new Uint8Array(this.wasmMemory.buffer, f.px, f.w * f.h * 4)];
  }
}

/** A decoder of the fake files: [w, h, fill] -> w x h pixels of fill; [0] fails. */
const fake: Decode = async (file) => {
  await tick();
  if (file[0] === 0) throw new Error("bad file");
  return {
    width: file[0]!,
    height: file[1]!,
    layout: LAYOUTS.BGRX!,
    data: new Uint8Array(file[0]! * file[1]! * 4).fill(file[2]!),
  };
};

test("each file listed is decoded into the room kura gives, a failure said so", async () => {
  const k = new FakeKura();
  k.files.set(1, { data: [2, 3, 7], listed: false, px: 0, w: 0, h: 0 });
  k.files.set(2, { data: [0], listed: false, px: 0, w: 0, h: 0 });
  const d = new ImageDecode(k, () => {}, fake, new Gate(), false, false);
  await tick(10);
  expect(k.done.sort((a, b) => a[0] - b[0])).toEqual([
    [1, 1],
    [2, 0],
  ]);
  expect(k.pixels(1)).toEqual(Array(24).fill(7));
  expect(d.count).toBe(2);
  // (listed once only)
  d.tick();
  await tick(10);
  expect(k.done.length).toBe(2);
});

test("nothing is listed or written while a state is taken: done once the gate opens", async () => {
  const k = new FakeKura();
  const gate = new Gate();
  const d = new ImageDecode(k, () => {}, fake, gate, false, false);
  await tick(5);
  const lists = k.lists;
  await gate.close();
  k.files.set(5, { data: [1, 1, 9], listed: false, px: 0, w: 0, h: 0 });
  d.tick();
  await tick(10);
  expect(k.lists).toBe(lists);
  expect(k.done).toEqual([]);
  gate.open();
  await tick(10);
  expect(k.done).toEqual([[5, 1]]);
  expect(k.pixels(5)).toEqual([9, 9, 9, 9]);
});

test("a browser whose decoder differs from Windows is never listed to", async () => {
  const k = new FakeKura();
  const notes: string[] = [];
  new ImageDecode(k, (l) => notes.push(l), fake, new Gate(), true, false);
  await tick(20);
  expect(k.lists).toBe(0);
  expect(notes.join()).toMatch(/does not give Windows' pixels/);
  expect(await decodesAsWindows(fake)).toBe(false);
  expect(CHECK_JPEG.startsWith("/9j/")).toBe(true);
});

test("planes laid out as kura::idec::Layout and as Chrome lays out 21x13 JPEGs", () => {
  // (Chrome's own copyTo layout of tests/winref's jpeg_420, jpeg_422, jpeg_444)
  expect(planes("I420", 21, 13)).toEqual([
    { offset: 0, stride: 21 },
    { offset: 273, stride: 11 },
    { offset: 350, stride: 11 },
  ]);
  expect(planesSize("I420", 21, 13)).toBe(427);
  expect(planesSize("I422", 21, 13)).toBe(559);
  expect(planesSize("I444", 21, 13)).toBe(819);
  expect(planesSize("BGRX", 21, 13)).toBe(21 * 13 * 4);
  expect(planes("RGBX", 3, 2)).toEqual([{ offset: 0, stride: 12 }]);
  // (Layout::size: w * h + 2 * ceil(w / 2) * ceil(h / 2) for 4:2:0)
  for (const [w, h] of [
    [1, 1],
    [2, 3],
    [5, 7],
    [64, 33],
  ] as const)
    expect(planesSize("I420", w, h)).toBe(w * h + 2 * ((w + 1) >> 1) * ((h + 1) >> 1));
});
