// Whether a part of memory is all zeros (snapz-worker.ts: blocks of zeros are left out of a
// save state). Four bytes at a time where aligned (BigUint64Array reads allocated a BigInt per
// word in V8; the scan is most of the time the program stands still for a state), the rest a
// byte at a time: a start that is not 8-aligned threw, and the last n & 7 bytes were not looked
// at (a block whose only non-zero bytes were there was dropped from the state).

export function zero(mem: Uint8Array, at: number, n: number): boolean {
  const end = at + n;
  let i = at;
  for (; i < end && (mem.byteOffset + i) & 3; i++) if (mem[i]) return false;
  const words = (end - i) >>> 2;
  if (words) {
    const w = new Uint32Array(mem.buffer, mem.byteOffset + i, words);
    for (let k = 0; k < words; k++) if (w[k] !== 0) return false;
  }
  for (i += words * 4; i < end; i++) if (mem[i]) return false;
  return true;
}
