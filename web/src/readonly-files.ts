/** Page-thread readFileSync cannot wait on fetch; preload before executing game scripts. */
export async function preloadFiles(
  root: string,
  paths: string[],
  request: typeof fetch = (input, init) => fetch(input, init),
): Promise<Map<string, Uint8Array>> {
  const files = new Map<string, Uint8Array>();
  let next = 0;
  const worker = async () => {
    while (next < paths.length) {
      const path = paths[next++]!;
      const r = await request(root + path.split("/").map(encodeURIComponent).join("/"));
      if (!r.ok) throw new Error(`ゲームファイル ${path}: HTTP ${r.status}`);
      files.set(path, new Uint8Array(await r.arrayBuffer()));
    }
  };
  await Promise.all(Array.from({ length: 4 }, worker));
  return files;
}
