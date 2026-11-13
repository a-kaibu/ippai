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
