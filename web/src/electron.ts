/**
 * Whether an Electron game's page runs with Node (`require`, `process`): its main script asks
 * for nodeIntegration, and the page shares its world with it (contextIsolation off, the default
 * before Electron 12). `main` is the main script, `version` the `version` file beside the exe.
 */
export function electronNode(main: string, version: string | null): boolean {
  // (the window's options; comments left out)
  const code = main.replace(/\/\*[\s\S]*?\*\/|(^|[^:\\])\/\/.*$/gm, "$1");
  const option = (name: string): boolean | null => {
    const values = [...code.matchAll(new RegExp(`["']?\\b${name}["']?\\s*:\\s*(true|false)\\b`, "g"))];
    return values.length ? values.every((v) => v[1] === "true") : null;
  };
  if (option("nodeIntegration") !== true) return false;
  const isolated = option("contextIsolation");
  if (isolated !== null) return !isolated;
  const major = /^\s*v?(\d+)\./.exec(version ?? "")?.[1];
  return major !== undefined && Number(major) < 12;
}

/**
 * The pages an Electron game's main script may open, as paths in its app folder, likeliest
 * first: the .html/.htm names it holds (loadFile, loadURL, path.join(__dirname, ...), or handed
 * to a library as SKYNovel's initRenderer), each beside the script (__dirname) and from the app
 * folder. `main` is the script's path in the app folder ("doc/app.js"), `code` its text.
 */
export function electronPages(main: string, code: string): string[] {
  const dir = main.replaceAll("\\", "/").split("/").slice(0, -1);
  const norm = (parts: string[]) => {
    const out: string[] = [];
    for (const s of parts)
      if (s === "..") {
        if (!out.length) return null;
        out.pop();
      } else if (s && s !== ".") out.push(s);
    return out.join("/");
  };
  const out: string[] = [];
  for (const m of code.matchAll(/(["'`])((?:(?!\1)[^\n\\])*?\.html?)\1/gi)) {
    const name = m[2]!.replace(/^file:\/\/+/i, "").replace(/^[^?#]*?\$\{[^}]*\}/, "");
    if (/^[a-z][a-z0-9+.-]*:/i.test(name) || name.includes("${")) continue;
    for (const p of [norm([...dir, ...name.split("/")]), norm(name.split("/"))])
      if (p && !out.includes(p)) out.push(p);
  }
  return out;
}
