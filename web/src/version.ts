/** The server's build (GET /version), shown small next to the name on the list page: an old
 * binary left on a server is plain to see. */
export interface Build {
  version: string;
  commit: string;
}

/** "v0.3.0 · 4f67fe6"; "" for anything else (an older server without /version answers the
 * page itself, which is not JSON of this shape). */
export function buildLabel(b: unknown): string {
  if (typeof b !== "object" || b === null) return "";
  const { version, commit } = b as Partial<Build>;
  if (typeof version !== "string" || !/^[0-9A-Za-z.+-]{1,32}$/.test(version)) return "";
  const c = typeof commit === "string" && /^[0-9a-f]{7,40}$/.test(commit) ? ` · ${commit.slice(0, 7)}` : "";
  return `v${version}${c}`;
}

export async function fetchBuild(): Promise<string> {
  try {
    const r = await fetch(new URL("version", location.href));
    return r.ok ? buildLabel(await r.json()) : "";
  } catch {
    return "";
  }
}
