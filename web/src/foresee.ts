// What each script of the game's trees names, read once by ippai serve ahead of any play
// (crates/ippai/src/foresee.rs, GET /foresee/<tree>), handed to kura at the start
// (vnhost_vfs_foresee): a script the game reads then foresees the pictures, voices and scripts
// it names, and what the scripts it goes on to name, whichever way the game goes, on its first
// run too. The server reads a tree the first time it is asked (202 meanwhile): asked again.

/** How often, and how many times, a table still being read is asked for again. */
export const FORESEE_RETRY_MS = 15000;
export const FORESEE_TRIES = 40;

/** The trees `mounts` ("<path>=<tree>", as IPPAI_MOUNTS has them) name, each once. */
export function mountedTrees(mounts: string[]): string[] {
  const out: string[] = [];
  for (const m of mounts) {
    const t = m.slice(m.indexOf("=") + 1);
    if (m.includes("=") && t && /^[A-Za-z0-9_.-]+$/.test(t) && !out.includes(t)) out.push(t);
  }
  return out;
}

export interface ForeseeModule {
  _malloc(n: number): number;
  _free(p: number): void;
  _vnhost_vfs_foresee?(text: number, len: number): void;
  wasmMemory: WebAssembly.Memory;
}

/** Hands table `text` to the module (copied there: kura parses it at once). */
export function handForesee(m: ForeseeModule, text: Uint8Array): boolean {
  if (!m._vnhost_vfs_foresee || !text.length) return false;
  const p = m._malloc(text.length) >>> 0;
  if (!p) return false;
  new Uint8Array(m.wasmMemory.buffer).set(text, p);
  m._vnhost_vfs_foresee(p, text.length);
  m._free(p);
  return true;
}

/** Each of `trees`' tables to `hand` once it is there: asked of `base` (the page's origin),
 * again while the server reads it; `wait` (tests) and `still` (false to stop) as given. */
export async function fetchForesee(
  trees: string[],
  hand: (tree: string, text: Uint8Array) => void,
  opt: {
    base?: string;
    fetch?: typeof fetch;
    wait?: (ms: number) => Promise<void>;
    still?: () => boolean;
  } = {},
): Promise<void> {
  const get = opt.fetch ?? fetch;
  const wait = opt.wait ?? ((ms) => new Promise((r) => setTimeout(r, ms)));
  const still = opt.still ?? (() => true);
  await Promise.all(
    trees.map(async (t) => {
      for (let i = 0; i < FORESEE_TRIES && still(); i++) {
        const r = await get(new URL(`foresee/${t}`, opt.base ?? location.href).href).catch(() => undefined);
        if (r?.status === 200) return hand(t, new Uint8Array(await r.arrayBuffer()));
        // (an older server, no such tree: nothing to wait for)
        if (r && r.status !== 202) return;
        await wait(FORESEE_RETRY_MS);
      }
    }),
  );
}
