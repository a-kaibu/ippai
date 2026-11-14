// Runs a wasm32-wasip1 test binary under node's WASI (cargo's runner in formal/wasm32.sh).
import { readFile } from "node:fs/promises";
import { WASI } from "node:wasi";
const [file, ...args] = process.argv.slice(2);
const wasi = new WASI({ version: "preview1", args: [file, ...args], env: process.env, preopens: { "/": "/" }, returnOnExit: true });
const inst = await WebAssembly.instantiate(await WebAssembly.compile(await readFile(file)), wasi.getImportObject());
process.exit(wasi.start(inst));
