// The game list (wine.html without ?run=): the games vnvm-pack add-game put in the store,
// served as one list by vnvm-server (GET /games). Picking one fills wine.html's parameters
// (run, mount, args, env) and starts it in the same page, from the same click (the audio and
// fullscreen need a user gesture).

/** From ErogameScape (批評空間) / VNDB: `vnvm-pack meta` (crates/vnvm-pack/src/meta.rs, Info) */
export interface GameMeta {
  source: string;
  egs_id?: number;
  vndb_id?: string;
  title: string;
  furigana?: string;
  brand?: string;
  /** yyyy-mm-dd */
  release?: string;
  /** 0-100 */
  score?: number;
  votes?: number;
  tags?: string[];
  series?: string;
  description?: string;
  links?: { label: string; url: string }[];
}

/** store/games/<id>.json (crates/vnvm-pack, GameEntry) */
export interface GameEntry {
  id: string;
  title: string;
  engine?: string;
  /** image file in store/games/ */
  cover?: string;
  run: string;
  args: string[];
  /** /path=tree */
  mounts: string[];
  /** NAME=value */
  env: string[];
  /** "x86_64": a 64-bit program */
  arch?: string;
  meta?: GameMeta;
}

const PLAYED = "vnvm-played";

/** When each game was last started here, by id (this browser only). */
function played(): Record<string, number> {
  try {
    return JSON.parse(localStorage.getItem(PLAYED) ?? "{}") as Record<string, number>;
  } catch {
    return {};
  }
}

function markPlayed(id: string): void {
  try {
    localStorage.setItem(PLAYED, JSON.stringify({ ...played(), [id]: Date.now() }));
  } catch {
    // (storage blocked: the order just doesn't remember)
  }
}

export async function fetchGames(): Promise<GameEntry[]> {
  const r = await fetch(new URL("games", location.href));
  if (!r.ok) throw new Error(`games: HTTP ${r.status}`);
  return (await r.json()) as GameEntry[];
}

export function coverUrl(g: GameEntry): string | null {
  return g.cover ? new URL(`store/games/${encodeURIComponent(g.cover)}`, location.href).href : null;
}

/** Sets wine.html's parameters for `g` (other parameters, e.g. &debug, stay). */
export function applyGame(params: URLSearchParams, g: GameEntry): void {
  params.set("run", g.run);
  params.delete("mount");
  for (const m of g.mounts) params.append("mount", m);
  if (g.args.length) params.set("args", g.args.join(","));
  else params.delete("args");
  for (const e of g.env) params.append("env", e);
  if (g.arch) params.set("arch", g.arch);
  else params.delete("arch");
  markPlayed(g.id);
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  cls?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function when(t: number): string {
  const d = new Date(t);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getMonth() + 1}/${d.getDate()} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** Shows the list in `root`; `pick` runs in the click that chose a game. */
export function showLibrary(root: HTMLElement, games: GameEntry[], pick: (g: GameEntry) => void): void {
  const last = played();
  // last played first, then by title
  const sorted = [...games].sort(
    (a, b) => (last[b.id] ?? 0) - (last[a.id] ?? 0) || a.title.localeCompare(b.title, "ja"),
  );
  root.replaceChildren();
  const head = el("header");
  head.append(el("h1", undefined, "vnvm"), el("span", "count", `${games.length} 本`));
  const search = el("input");
  search.type = "search";
  search.placeholder = "タイトル・ブランド・タグで絞り込み";
  search.setAttribute("aria-label", "タイトル・ブランド・タグで絞り込み");
  if (games.length > 6) head.append(search);
  // the runs' logs (crash reports: web/src/crashlog.ts)
  const logs = el("a", "logs", "ログ");
  logs.href = "logs.html";
  head.append(logs);
  root.append(head);
  if (!games.length) {
    const empty = el("p", "empty");
    empty.append(
      "ゲームがまだありません。ゲームのフォルダを ",
      el("code", undefined, "vnvm-pack tree <dir> <tree>"),
      " で入れて、",
      el("code", undefined, "vnvm-pack add-game <id> --title ... --tree <tree> --exe <exe>"),
      " で一覧に追加します。",
    );
    root.append(empty);
  }
  const grid = el("ul", "grid");
  const cards = sorted.map((g) => {
    const li = el("li");
    const b = el("button", "card");
    b.type = "button";
    const art = el("div", "cover");
    const url = coverUrl(g);
    if (url) {
      const img = el("img");
      img.src = url;
      img.alt = "";
      img.loading = "lazy";
      art.append(img);
    } else {
      art.classList.add("blank");
      art.append(el("span", undefined, g.title));
    }
    const m = g.meta;
    const meta = el("div", "meta");
    meta.append(el("div", "title", g.title));
    if (m?.brand || m?.release) {
      meta.append(el("div", "brand", [m.brand, m.release?.slice(0, 4)].filter(Boolean).join(" · ")));
    }
    const sub = el("div", "sub");
    if (g.engine) sub.append(el("span", "engine", g.engine));
    if (m?.score) sub.append(el("span", "score", `★${m.score}`));
    if (last[g.id]) sub.append(el("span", "played", `前回 ${when(last[g.id]!)}`));
    meta.append(sub);
    b.append(art, meta);
    b.addEventListener("click", () => pick(g));
    li.append(b);
    if (m) {
      const info = el("button", "info", "i");
      info.type = "button";
      info.title = "ゲーム情報";
      info.setAttribute("aria-label", `${g.title} の情報`);
      info.addEventListener("click", () => showInfo(g, pick));
      li.append(info);
    }
    grid.append(li);
    const key = [g.title, g.engine, g.id, m?.title, m?.furigana, m?.brand, m?.series, ...(m?.tags ?? [])];
    return { li, key: key.filter(Boolean).join(" ").toLowerCase() };
  });
  search.addEventListener("input", () => {
    const q = search.value.trim().toLowerCase();
    for (const c of cards) c.li.hidden = !!q && !c.key.includes(q);
  });
  root.append(grid);
  root.hidden = false;
}

/** A game's metadata (cover, brand, release, score, tags, story, links) with a start button. */
function showInfo(g: GameEntry, pick: (g: GameEntry) => void): void {
  const m = g.meta!;
  const d = el("dialog", "gameinfo");
  const close = el("button", "close", "×");
  close.type = "button";
  close.setAttribute("aria-label", "閉じる");
  close.addEventListener("click", () => d.close());
  const url = coverUrl(g);
  if (url) {
    const img = el("img", "art");
    img.src = url;
    img.alt = "";
    d.append(img);
  }
  const body = el("div", "body");
  body.append(el("h2", undefined, m.title || g.title));
  if (m.furigana) body.append(el("div", "furigana", m.furigana));
  const facts = el("dl");
  const fact = (k: string, v?: string) => {
    if (v) facts.append(el("dt", undefined, k), el("dd", undefined, v));
  };
  fact("ブランド", m.brand);
  fact("発売日", m.release);
  fact("シリーズ", m.series);
  if (m.score)
    fact(m.source === "vndb" ? "VNDB評価" : "中央値", `${m.score}${m.votes ? `（${m.votes}票）` : ""}`);
  fact("エンジン", g.engine);
  body.append(facts);
  if (m.tags?.length) {
    const tags = el("ul", "tags");
    for (const t of m.tags) tags.append(el("li", undefined, t));
    body.append(tags);
  }
  if (m.description) body.append(el("p", "story", m.description));
  if (m.links?.length) {
    const links = el("div", "links");
    for (const l of m.links) {
      const a = el("a", undefined, l.label);
      a.href = l.url;
      a.target = "_blank";
      a.rel = "noopener noreferrer";
      links.append(a);
    }
    body.append(links);
  }
  const start = el("button", "start", "起動");
  start.type = "button";
  start.addEventListener("click", () => {
    d.close();
    pick(g);
  });
  body.append(start);
  d.append(close, body);
  d.addEventListener("close", () => d.remove());
  // a click on the backdrop closes it
  d.addEventListener("click", (e) => {
    if (e.target === d) d.close();
  });
  document.body.append(d);
  d.showModal();
}
