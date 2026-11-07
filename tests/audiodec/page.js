// The page of tests/audiodec/run.mjs: decodes the same streams with crates/vorbis (wasm, as kura
// would in the browser) and with WebCodecs' AudioDecoder, and says how fast and how far apart.

/** Packets of an Ogg stream (one logical stream), in order. */
export function oggPackets(b) {
  const out = [];
  let cur = [];
  let o = 0;
  while (o + 27 <= b.length) {
    if (b[o] !== 0x4f || b[o + 1] !== 0x67 || b[o + 2] !== 0x67 || b[o + 3] !== 0x53) throw new Error(`no OggS at ${o}`);
    const nseg = b[o + 26];
    const lace = b.subarray(o + 27, o + 27 + nseg);
    let p = o + 27 + nseg;
    for (const l of lace) {
      cur.push(b.subarray(p, p + l));
      p += l;
      if (l < 255) {
        const n = cur.reduce((a, s) => a + s.length, 0);
        const pk = new Uint8Array(n);
        let k = 0;
        for (const s of cur) {
          pk.set(s, k);
          k += s.length;
        }
        out.push(pk);
        cur = [];
      }
    }
    o = p;
  }
  return out;
}

/** MPEG audio frames (layer 3), ID3 skipped. */
export function mp3Frames(b) {
  let o = 0;
  if (b[0] === 0x49 && b[1] === 0x44 && b[2] === 0x33) o = 10 + ((b[6] << 21) | (b[7] << 14) | (b[8] << 7) | b[9]);
  const rates = [44100, 48000, 32000];
  const kbps = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
  const out = [];
  let rate = 0;
  while (o + 4 <= b.length) {
    if (b[o] !== 0xff || (b[o + 1] & 0xe0) !== 0xe0) {
      o++;
      continue;
    }
    const br = kbps[b[o + 2] >> 4];
    rate = rates[(b[o + 2] >> 2) & 3];
    const pad = (b[o + 2] >> 1) & 1;
    if (!br || !rate) {
      o++;
      continue;
    }
    const len = Math.floor((144000 * br) / rate) + pad;
    out.push(b.subarray(o, o + len));
    o += len;
  }
  return { frames: out, rate };
}

function xiphLace(n) {
  const a = [];
  while (n >= 255) {
    a.push(255);
    n -= 255;
  }
  a.push(n);
  return a;
}

/** A stream as AudioDecoder takes it: config, the packets to decode, frames per packet guess. */
export function webcodecsStream(name, bytes) {
  if (name.endsWith(".mp3")) {
    const { frames, rate } = mp3Frames(bytes);
    return { config: { codec: "mp3", sampleRate: rate, numberOfChannels: 2 }, packets: frames, spf: 1152 };
  }
  const pk = oggPackets(bytes);
  if (String.fromCharCode(...pk[0].subarray(1, 7)) === "vorbis") {
    const [id, comment, setup] = pk;
    const dv = new DataView(id.buffer, id.byteOffset);
    const channels = id[11];
    const rate = dv.getUint32(12, true);
    const desc = new Uint8Array([2, ...xiphLace(id.length), ...xiphLace(comment.length), ...id, ...comment, ...setup]);
    return { config: { codec: "vorbis", sampleRate: rate, numberOfChannels: channels, description: desc }, packets: pk.slice(3), heads: [id, setup], spf: 1024 };
  }
  if (String.fromCharCode(...pk[0].subarray(0, 8)) === "OpusHead") {
    const head = pk[0];
    return { config: { codec: "opus", sampleRate: 48000, numberOfChannels: head[9], description: head }, packets: pk.slice(2), spf: 960 };
  }
  throw new Error(`${name}: not Vorbis, Opus or MP3`);
}

/** Interleaved f32 of an AudioData. */
function interleaved(d) {
  const ch = d.numberOfChannels;
  const n = d.numberOfFrames;
  const out = new Float32Array(n * ch);
  if (d.format === "f32") {
    d.copyTo(out, { planeIndex: 0, format: "f32" });
    return out;
  }
  const plane = new Float32Array(n);
  for (let c = 0; c < ch; c++) {
    d.copyTo(plane, { planeIndex: c, format: "f32-planar" });
    for (let i = 0; i < n; i++) out[i * ch + c] = plane[i];
  }
  return out;
}

function concat(parts) {
  const n = parts.reduce((a, p) => a + p.length, 0);
  const out = new Float32Array(n);
  let k = 0;
  for (const p of parts) {
    out.set(p, k);
    k += p.length;
  }
  return out;
}

/** Whole stream through AudioDecoder: samples, wall ms, main-thread ms in its callbacks. */
export async function decodeWebcodecs(s) {
  const parts = [];
  let cb = 0;
  let err = null;
  const dec = new AudioDecoder({
    output: (d) => {
      const t = performance.now();
      parts.push(interleaved(d));
      d.close();
      cb += performance.now() - t;
    },
    error: (e) => {
      err = e;
    },
  });
  dec.configure(s.config);
  const t0 = performance.now();
  let ts = 0;
  for (const p of s.packets) {
    dec.decode(new EncodedAudioChunk({ type: "key", timestamp: ts, data: p }));
    ts += Math.round((s.spf * 1e6) / s.config.sampleRate);
  }
  await dec.flush();
  const wall = performance.now() - t0;
  dec.close();
  if (err) throw err;
  return { pcm: concat(parts), wall, cb };
}

/**
 * One packet at a time, each waited for (what a guest thread that wants a packet's samples now
 * would see): the time from decode() to its output, per packet.
 */
export async function latencyWebcodecs(s, n) {
  let resolve = null;
  const dec = new AudioDecoder({
    output: (d) => {
      d.close();
      resolve?.();
    },
    error: () => resolve?.(),
  });
  dec.configure(s.config);
  const times = [];
  let ts = 0;
  for (const p of s.packets.slice(0, n)) {
    const t = performance.now();
    const got = new Promise((r) => {
      resolve = r;
      setTimeout(r, 200);
    });
    dec.decode(new EncodedAudioChunk({ type: "key", timestamp: ts, data: p }));
    ts += Math.round((s.spf * 1e6) / s.config.sampleRate);
    await got;
    times.push(performance.now() - t);
  }
  dec.close();
  times.sort((a, b) => a - b);
  return { p50: times[times.length >> 1], p95: times[Math.floor(times.length * 0.95)], max: times[times.length - 1] };
}

/** Whole stream through crates/vorbis in wasm: samples, ms. */
export function decodeWasm(w, s) {
  const e = w.exports;
  const put = (b) => {
    const p = e.alloc(b.length);
    new Uint8Array(e.memory.buffer, p, b.length).set(b);
    return p;
  };
  const [id, setup] = s.heads;
  const ip = put(id);
  const sp = put(setup);
  if (!e.open(ip, id.length, sp, setup.length)) throw new Error("wasm: no decoder");
  e.free(ip, id.length);
  e.free(sp, setup.length);
  // (packets copied in before the clock starts: the guest has them in memory)
  const ptrs = s.packets.map((p) => [put(p), p.length]);
  const t0 = performance.now();
  for (const [p, n] of ptrs) e.packet(p, n);
  const ms = performance.now() - t0;
  const pcm = new Float32Array(e.memory.buffer, e.out_ptr(), e.out_len()).slice();
  for (const [p, n] of ptrs) e.free(p, n);
  e.out_clear();
  return { pcm, ms };
}

/** How far apart two decodes are: max |a-b|, SNR in dB of b against a, lengths. */
export function compare(a, b) {
  const n = Math.min(a.length, b.length);
  let max = 0;
  let sig = 0;
  let noise = 0;
  for (let i = 0; i < n; i++) {
    const d = a[i] - b[i];
    max = Math.max(max, Math.abs(d));
    sig += a[i] * a[i];
    noise += d * d;
  }
  return { max, snr: noise === 0 ? Infinity : 10 * Math.log10(sig / noise), na: a.length, nb: b.length };
}

/** FLAC's description: "fLaC" and a STREAMINFO block (4096-sample blocks, 44.1 kHz, stereo, 16 bits). */
function flacDescription() {
  const d = new Uint8Array(42);
  d.set([0x66, 0x4c, 0x61, 0x43, 0x80, 0, 0, 34, 0x10, 0, 0x10, 0]);
  const v = (0x0ac44n << 44n) | (1n << 41n) | (15n << 36n);
  for (let i = 0; i < 8; i++) d[18 + i] = Number((v >> BigInt(56 - 8 * i)) & 0xffn);
  return d;
}

/** Codecs AudioDecoder is asked about (Vorbis's description is a stream's own headers: run.mjs). */
const CODECS = [
  { codec: "flac", sampleRate: 44100, numberOfChannels: 2, description: flacDescription() },
  { codec: "opus", sampleRate: 48000, numberOfChannels: 2 },
  { codec: "mp3", sampleRate: 44100, numberOfChannels: 2 },
  { codec: "mp4a.40.2", sampleRate: 44100, numberOfChannels: 2 },
  { codec: "mp4a.69", sampleRate: 44100, numberOfChannels: 2 },
  { codec: "alaw", sampleRate: 8000, numberOfChannels: 1 },
  { codec: "ulaw", sampleRate: 8000, numberOfChannels: 1 },
  { codec: "pcm-s16", sampleRate: 44100, numberOfChannels: 2 },
  // what VN movies and sound files also hold
  { codec: "mp2", sampleRate: 44100, numberOfChannels: 2 },
  { codec: "wma", sampleRate: 44100, numberOfChannels: 2 },
  { codec: "adpcm_ms", sampleRate: 22050, numberOfChannels: 1 },
];

export async function support(extra = []) {
  const out = {};
  for (const c of [...extra, ...CODECS]) {
    try {
      out[c.codec] = (await AudioDecoder.isConfigSupported(c)).supported;
    } catch {
      out[c.codec] = false;
    }
  }
  return out;
}
