// The shape of what koji makes, for JavaScript engines' wasm (tests/bench/js.mjs runs it in node
// and in JavaScriptCore's shell): wasm modules generated at run time, translated blocks sharing
// one memory and one table of functions, calls through that table, code that gets hot later.
// Prints "BENCH <metric> <ms>" (lower is better), "BENCH-CHECK" lines and "BENCH-END".
//
//   compile        a 2000-function module (translated blocks: arithmetic on locals, loads and
//                  stores in the shared memory) compiled (new WebAssembly.Module)
//   instantiate    ... instantiated with the shared memory and table
//   first_calls    every function of it called once (engines that compile lazily do it here)
//   small_modules  400 one-function modules compiled, instantiated and put in the table, one by
//                  one (a JIT that emits a module per block)
//   call_direct    10M calls of a small function with `call`
//   call_indirect  the same through the table (`call_indirect`, 8 targets), as koji's dispatch
//   tier_first     the first call of a fresh hot loop (the engine's first tier)
//   tier_steady    the same call after 2 s of calling it (the optimizing tier)
/* global print, preciseTime */
const say = typeof print === "function" && typeof process === "undefined" ? print : (s) => console.log(s);
const now = typeof performance !== "undefined" ? () => performance.now() : () => preciseTime() * 1000;

// ---- a small wasm encoder ----
const uleb = (n) => {
  const b = [];
  do {
    let x = n & 0x7f;
    n >>>= 7;
    if (n) x |= 0x80;
    b.push(x);
  } while (n);
  return b;
};
const sleb = (n) => {
  const b = [];
  for (;;) {
    const x = n & 0x7f;
    n >>= 7;
    if ((n === 0 && !(x & 0x40)) || (n === -1 && x & 0x40)) return b.push(x), b;
    b.push(x | 0x80);
  }
};
const str = (s) => [...uleb(s.length), ...[...s].map((c) => c.charCodeAt(0))];
const vec = (items) => [...uleb(items.length), ...items.flat()];
const section = (id, body) => [id, ...uleb(body.length), ...body];
const I32 = 0x7f;
const op = {
  get: (i) => [0x20, ...uleb(i)], set: (i) => [0x21, ...uleb(i)], tee: (i) => [0x22, ...uleb(i)],
  i32: (v) => [0x41, ...sleb(v | 0)], add: [0x6a], sub: [0x6b], mul: [0x6c], and: [0x71], xor: [0x73],
  shl: [0x74], shru: [0x76], rotl: [0x77], ltu: [0x49], eqz: [0x45],
  load: [0x28, 2, 0], store: [0x36, 2, 0], call: (f) => [0x10, ...uleb(f)], calli: (t) => [0x11, ...uleb(t), 0],
  loop: [0x03, 0x40], block: [0x02, 0x40], br: (d) => [0x0c, ...uleb(d)], brif: (d) => [0x0d, ...uleb(d)], end: [0x0b],
};

/** a module: { types: [[params, results]], imports: bool (memory "m", table "t"), funcs: [{type, locals, code}],
 *  exports: [[name, func]], table: n (own table, with elems: [funcs]) } */
const put = (b, a) => {
  for (const x of a) b.push(x);
};
function module({ types, imports = false, funcs, exports = [], table = 0, elems = [] }) {
  const b = [0x00, 0x61, 0x73, 0x6d, 1, 0, 0, 0];
  put(b, section(1, vec(types.map(([p, r]) => [0x60, ...vec(p.map((t) => [t])), ...vec(r.map((t) => [t]))]))));
  if (imports)
    put(b, section(2, vec([[...str("e"), ...str("m"), 2, 0, ...uleb(4)], [...str("e"), ...str("t"), 1, 0x70, 0, ...uleb(1024)]])));
  put(b, section(3, vec(funcs.map((f) => uleb(f.type)))));
  if (table) put(b, section(4, vec([[0x70, 0, ...uleb(table)]])));
  put(b, section(7, vec(exports.map(([n, f]) => [...str(n), 0, ...uleb(f)]))));
  if (elems.length) put(b, section(9, vec([[0, ...op.i32(0), ...op.end, ...vec(elems.map((f) => uleb(f)))]])));
  put(b, section(10, vec(funcs.map((f) => {
    const body = [...vec(f.locals ? [[...uleb(f.locals), I32]] : []), ...f.code, ...op.end];
    return [...uleb(body.length), ...body];
  }))));
  return new Uint8Array(b);
}

// a translated block: (param x) (result i32), locals a b c; `n` steps of arithmetic and memory
let seed = 1;
const rnd = () => ((seed = (Math.imul(seed, 1103515245) + 12345) | 0) >>> 8);
function block(n) {
  const c = [...op.get(0), ...op.set(1)];
  for (let i = 0; i < n; i++) {
    const k = rnd();
    c.push(...op.get(1), ...op.get(0), ...op.i32(k | 1), ...op.mul, ...op.add, ...op.get(2), ...op.i32(k % 31), ...op.rotl, ...op.xor, ...op.set(1));
    c.push(...op.get(1), ...op.i32(0xfffc), ...op.and, ...op.load, ...op.get(3), ...op.add, ...op.set(2));
    c.push(...op.get(2), ...op.i32(0xfffc), ...op.and, ...op.i32(0x10000), ...op.add, ...op.get(1), ...op.store);
    c.push(...op.get(3), ...op.get(2), ...op.sub, ...op.i32(k), ...op.xor, ...op.set(3));
  }
  c.push(...op.get(1), ...op.get(2), ...op.xor, ...op.get(3), ...op.xor);
  return { type: 0, locals: 3, code: c };
}

const memory = new WebAssembly.Memory({ initial: 4 });
const table = new WebAssembly.Table({ initial: 1024, element: "anyfunc" });
new Uint32Array(memory.buffer).forEach((_, i, a) => (a[i] = Math.imul(i, 2654435761)));
const env = { e: { m: memory, t: table } };
const fnv = (h, v) => Math.imul(h ^ v, 16777619) >>> 0;

// ---- compile, instantiate, first calls ----
{
  const N = 2000;
  const funcs = [];
  for (let i = 0; i < N; i++) funcs.push(block(12 + (i % 24)));
  const bytes = module({ types: [[[I32], [I32]]], imports: true, funcs, exports: funcs.map((_, i) => [`f${i}`, i]) });
  let t = now();
  const m = new WebAssembly.Module(bytes);
  say(`BENCH compile ${(now() - t).toFixed(4)}`);
  t = now();
  const inst = new WebAssembly.Instance(m, env);
  say(`BENCH instantiate ${(now() - t).toFixed(4)}`);
  t = now();
  let h = 2166136261;
  for (let i = 0; i < N; i++) h = fnv(h, inst.exports[`f${i}`](i));
  say(`BENCH first_calls ${(now() - t).toFixed(4)}`);
  say(`BENCH-CHECK compile ${h.toString(16).padStart(8, "0")}`);
}

// ---- a module per block ----
{
  const N = 400;
  const all = [];
  for (let i = 0; i < N; i++) all.push(module({ types: [[[I32], [I32]]], imports: true, funcs: [block(20)], exports: [["f", 0]] }));
  const t = now();
  for (let i = 0; i < N; i++) table.set(i, new WebAssembly.Instance(new WebAssembly.Module(all[i]), env).exports.f);
  say(`BENCH small_modules ${(now() - t).toFixed(4)}`);
  let h = 2166136261;
  for (let i = 0; i < N; i++) h = fnv(h, table.get(i)(i));
  say(`BENCH-CHECK small_modules ${h.toString(16).padStart(8, "0")}`);
}

// ---- call vs call_indirect ----
{
  // functions 0..7: leaves x*k+i; 8: direct loop; 9: indirect loop (target i & 7)
  const leaves = [];
  for (let i = 0; i < 8; i++) leaves.push({ type: 0, code: [...op.get(0), ...op.i32(2 * i + 3), ...op.mul, ...op.i32(i + 1), ...op.add] });
  // (param n) (result acc): locals i acc
  const driver = (indirect) => ({
    type: 0, locals: 2,
    code: [
      ...op.i32(1), ...op.set(2),
      ...op.block, ...op.loop,
      ...op.get(1), ...op.get(0), ...op.ltu, ...op.eqz, ...op.brif(1),
      ...op.get(2), ...(indirect ? [...op.get(1), ...op.i32(7), ...op.and, ...op.calli(0)] : op.call(0)), ...op.set(2),
      ...op.get(1), ...op.i32(1), ...op.add, ...op.set(1),
      ...op.br(0), ...op.end, ...op.end,
      ...op.get(2),
    ],
  });
  const bytes = module({
    types: [[[I32], [I32]]], funcs: [...leaves, driver(false), driver(true)],
    exports: [["direct", 8], ["indirect", 9]], table: 8, elems: [0, 1, 2, 3, 4, 5, 6, 7],
  });
  const e = new WebAssembly.Instance(new WebAssembly.Module(bytes), {}).exports;
  e.direct(100000), e.indirect(100000);
  let t = now();
  const d = e.direct(10000000);
  say(`BENCH call_direct ${(now() - t).toFixed(4)}`);
  t = now();
  const ind = e.indirect(10000000);
  say(`BENCH call_indirect ${(now() - t).toFixed(4)}`);
  say(`BENCH-CHECK calls ${fnv(fnv(2166136261, d), ind).toString(16).padStart(8, "0")}`);
}

// ---- tiers ----
{
  // (param n): n times a 30-step block's work
  const b = block(30);
  const hot = {
    type: 0, locals: 4,
    code: [
      ...op.block, ...op.loop,
      ...op.get(4), ...op.get(0), ...op.ltu, ...op.eqz, ...op.brif(1),
      ...b.code.slice(4, -8),
      ...op.get(4), ...op.i32(1), ...op.add, ...op.set(4),
      ...op.br(0), ...op.end, ...op.end,
      ...op.get(1), ...op.get(2), ...op.xor, ...op.get(3), ...op.xor,
    ],
  };
  // (block()'s code without its a = x (4 bytes) and its a ^ b ^ c (8 bytes): the loop keeps a b c
  // from one turn to the next, local 4 counts)
  const bytes = module({ types: [[[I32], [I32]]], imports: true, funcs: [hot], exports: [["hot", 0]] });
  const f = new WebAssembly.Instance(new WebAssembly.Module(bytes), env).exports.hot;
  const N = 20000;
  let t = now();
  const first = f(N) >>> 0;
  say(`BENCH tier_first ${(now() - t).toFixed(4)}`);
  // (the first result only: the memory the later calls start from depends on how many ran)
  say(`BENCH-CHECK tier ${first.toString(16).padStart(8, "0")}`);
  let h = first;
  const until = now() + 2000;
  while (now() < until) h = fnv(h, f(N));
  const times = [];
  for (let i = 0; i < 9; i++) {
    t = now();
    h = fnv(h, f(N));
    times.push(now() - t);
  }
  times.sort((x, y) => x - y);
  say(`BENCH tier_steady ${times[4].toFixed(4)}`);
  if (!h) say("(never)");
}
say("BENCH-END");
