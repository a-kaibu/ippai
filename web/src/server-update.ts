// The server updating itself to the newest release (masu serve: GET / POST /api/update,
// crates/masu/src/update.rs), from 設定's サーバー section: ask what is there, install it, wait
// until the restarted server answers with the new version, then reload the page.

export interface UpdateInfo {
  /** the running version */
  current: string;
  /** the newest release, "" when it could not be asked */
  latest: string;
  newer: boolean;
  /** the server can replace itself (Linux) */
  supported: boolean;
  /** why the newest release is not known */
  error: string;
}

const str = (v: unknown) => (typeof v === "string" ? v : "");

/** GET /api/update's answer, or null when it is not one (an older server answers with the page). */
export function parseUpdate(v: unknown): UpdateInfo | null {
  if (typeof v !== "object" || v === null) return null;
  const o = v as Record<string, unknown>;
  if (typeof o.current !== "string" || typeof o.newer !== "boolean") return null;
  return {
    current: o.current,
    latest: str(o.latest),
    newer: o.newer,
    supported: o.supported !== false,
    error: str(o.error),
  };
}

/** "v0.3.4" and "0.3.4" are one version */
export const sameVersion = (a: string, b: string): boolean =>
  !!a && a.replace(/^v/, "") === b.replace(/^v/, "");

type Fetch = (url: URL, init?: RequestInit) => Promise<Response>;
const at = (path: string) => new URL(path, location.href);

export async function checkUpdate(f: Fetch = fetch): Promise<UpdateInfo> {
  const r = await f(at("api/update"), { cache: "no-store" });
  if (!r.ok) throw new Error(r.status === 404 ? "このサーバーは更新に対応していません" : `HTTP ${r.status}`);
  const info = parseUpdate(await r.json().catch(() => null));
  if (!info) throw new Error("このサーバーは更新に対応していません");
  return info;
}

/** Asks the server to install the newest release; the version it installed. */
export async function installUpdate(f: Fetch = fetch): Promise<string> {
  const r = await f(at("api/update"), { method: "POST" });
  if (!r.ok) {
    // (plain text: 409 「最新です」 or 「更新中です」, 502 「更新できませんでした: …」)
    const text = (await r.text().catch(() => "")).trim().slice(0, 300);
    if (r.status === 501)
      throw new Error("このサーバーでは自動更新できません（Linux の amd64 / arm64 のみ）");
    if (r.status === 409) throw new Error(text || "最新です");
    throw new Error(
      text.startsWith("更新") ? text : `更新できませんでした（HTTP ${r.status}${text ? `: ${text}` : ""}）`,
    );
  }
  const v = str(((await r.json().catch(() => ({}))) as { installed?: unknown }).installed);
  if (!v) throw new Error("更新の結果を読めませんでした");
  return v;
}

/** Until GET /version answers with `version` (the restarted server): true, or false after
 * `timeoutMs`. The old server may still answer for a moment, and the address none while it
 * restarts. */
export async function waitForVersion(
  version: string,
  {
    f = fetch as Fetch,
    timeoutMs = 60_000,
    everyMs = 500,
    sleep = (ms: number) => new Promise((r) => setTimeout(r, ms)),
    now = () => Date.now(),
  } = {},
): Promise<boolean> {
  const end = now() + timeoutMs;
  while (now() < end) {
    try {
      const r = await f(at("version"), { cache: "no-store" });
      if (r.ok && sameVersion(str(((await r.json()) as { version?: unknown }).version), version)) return true;
    } catch {
      // (restarting)
    }
    await sleep(everyMs);
  }
  return false;
}
