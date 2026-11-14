// A game's metadata from a page the user pastes (批評空間 / VNDB): masu serve fetches it in the
// background and keeps the result in its database (crates/masu/src/library_api.rs,
// PUT/GET /api/library/<game>/meta); the list (GET /games) then shows it.

/** The state of the last fetch (crates/masu/src/library.rs, MetaStatus). */
export interface MetaStatus {
  url: string;
  state: "pending" | "ok" | "error";
  error: string;
  updated_ms: number;
}

// (1 to 9 digits naming a number over 0, as meta_url.rs `number`)
const id = (s: string | null | undefined) => !!s && /^\d{1,9}$/.test(s) && Number(s) > 0;

/** Whether `s` looks like a page the server takes (it checks again: meta_url.rs). */
export function looksLikeGamePage(s: string): boolean {
  const t = s.trim();
  if (t.startsWith("egs:")) return id(t.slice(4));
  if (t.startsWith("vndb:")) return /^v/i.test(t.slice(5)) && id(t.slice(6));
  let u: URL;
  try {
    u = new URL(/^https?:\/\//i.test(t) ? t : `https://${t}`);
  } catch {
    return false;
  }
  if (!/^https?:$/.test(u.protocol) || u.username || u.password) return false;
  const host = u.hostname.replace(/\.$/, "").replace(/^www\./, "");
  if (host === "erogamescape.dyndns.org" || host === "erogamescape.org") {
    const game = u.search
      .slice(1)
      .split("&")
      .find((p) => p.startsWith("game="));
    return u.pathname.endsWith("/game.php") && id(game?.slice(5));
  }
  if (host === "vndb.org") {
    const first = u.pathname.split("/")[1] ?? "";
    return /^v/i.test(first) && id(first.slice(1));
  }
  return false;
}

/** What the page says about the last fetch. */
export function describe(s: MetaStatus | null): string {
  if (!s) return "";
  if (s.state === "pending") return "取得しています…";
  if (s.state === "error") return `取得できませんでした：${s.error}`;
  return `${new Date(s.updated_ms).toLocaleString("ja-JP")} に更新しました`;
}

function endpoint(game: string): URL {
  return new URL(`api/library/${encodeURIComponent(game)}/meta`, location.href);
}

async function failure(r: Response): Promise<Error> {
  const text = (await r.text().catch(() => "")).trim();
  if (r.status === 404) return new Error("このゲームはサーバーの一覧にありません");
  return new Error(text && text.length < 200 ? text : `サーバー: HTTP ${r.status}`);
}

export async function metaStatus(game: string): Promise<MetaStatus | null> {
  const r = await fetch(endpoint(game));
  if (!r.ok) throw await failure(r);
  return (await r.json()) as MetaStatus | null;
}

/** Starts the fetch of `url` for `game`. */
export async function fetchMeta(game: string, url: string): Promise<MetaStatus> {
  const r = await fetch(endpoint(game), {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ url }),
  });
  if (!r.ok) throw await failure(r);
  return (await r.json()) as MetaStatus;
}

/** Polls until the fetch ends (or `signal` aborts); every state seen goes to `seen`. */
export async function untilSettled(
  game: string,
  seen: (s: MetaStatus | null) => void,
  signal: AbortSignal,
  every = 1000,
  read: (game: string) => Promise<MetaStatus | null> = metaStatus,
): Promise<MetaStatus | null> {
  for (;;) {
    await new Promise((ok) => setTimeout(ok, every));
    if (signal.aborted) return null;
    const s = await read(game);
    seen(s);
    if (!s || s.state !== "pending") return s;
  }
}

/** An ErogameScape game like this one (crates/masu/src/library_api.rs candidates). */
export interface Candidate {
  id: number;
  name: string;
  /** 0-1 */
  score: number;
  votes: number;
  /** the one taken without asking */
  best: boolean;
}

/** The ErogameScape games most like `game`'s names, or `q`. */
export async function candidates(game: string, q = ""): Promise<Candidate[]> {
  const url = new URL(`api/library/${encodeURIComponent(game)}/candidates`, location.href);
  if (q.trim()) url.searchParams.set("q", q.trim());
  const r = await fetch(url);
  if (!r.ok) throw await failure(r);
  return (await r.json()) as Candidate[];
}

/** A candidate's page on ErogameScape. */
export const egsPage = (id: number) =>
  `https://erogamescape.dyndns.org/~ap2/ero/toukei_kaiseki/game.php?game=${id}`;
