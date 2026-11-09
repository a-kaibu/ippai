// The game list (play.html without ?run=): a header (name, search, 設定 at the top right: a
// sidebar with tabs, ui/Sidebar.tsx), the game last played to continue, the cards with cover, brand, year
// and score from `ippai meta` (ordered and split into sections by brand or year as chosen:
// library-view.ts), and a sidebar with the game's details and records. Picking a game starts it from that click
// (audio and fullscreen need the user gesture).

import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  Download,
  ExternalLink,
  HardDrive,
  Heart,
  Info,
  NotebookPen,
  Play,
  RefreshCw,
  Search,
  PanelRightClose,
  PanelRightOpen,
  Star,
  Trash2,
  TriangleAlert,
  WifiOff,
  X,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";
import { coverUrl, fetchGames, played, type GameEntry } from "../launcher";
import {
  candidates,
  describe,
  egsPage,
  fetchMeta,
  looksLikeGamePage,
  metaStatus,
  untilSettled,
  type Candidate,
  type MetaStatus,
} from "../meta-source";
import { api } from "../library-data";
import {
  brandOf,
  GROUPS,
  groupGames,
  orderGames,
  parseView,
  SORTS,
  sortGames,
  visibleGames,
  when,
  type Filter,
  type GroupKey,
  type ListView,
  type SortKey,
} from "../library-view";
import { dropOffline, offlineState, saveOffline, type OfflineState } from "../offline";
import { useApp } from "../pwa";
import { fetchBuild } from "../version";
import { Backup, Note, useProfile } from "./Personal";
import { Sidebar, type SideTab } from "./Sidebar";
import { renderShell } from "./Shell";

/** The app's mark: the sake bottle (the same as the favicon and app icons). */
export function Logo({ className }: { className?: string }) {
  return (
    <img
      src="./icons/icon-192.png"
      alt=""
      aria-hidden
      draggable={false}
      className={cn("size-7 select-none", className)}
    />
  );
}

function Cover({ game, className }: { game: GameEntry; className?: string }) {
  const url = coverUrl(game);
  return url ? (
    <img src={url} alt="" loading="lazy" className={cn("size-full object-cover", className)} />
  ) : (
    <div className="grid size-full place-items-center bg-gradient-to-br from-secondary via-card to-background p-3 text-center font-semibold text-balance text-muted-foreground">
      {game.title}
    </div>
  );
}

/** A brand's name that narrows the list to that brand when clicked. */
function BrandLink({
  game,
  onBrand,
  className,
}: {
  game: GameEntry;
  onBrand: (b: string) => void;
  className?: string;
}) {
  const b = brandOf(game);
  if (!b) return null;
  return (
    <button
      type="button"
      onClick={() => onBrand(b)}
      title={`${b} のゲームだけ表示`}
      data-brand={b}
      className={cn(
        "inline rounded-sm text-left underline-offset-2 outline-none hover:text-foreground hover:underline focus-visible:ring-[3px] focus-visible:ring-ring/50",
        className,
      )}
    >
      {b}
    </button>
  );
}

/** "Key · 2004" with the brand clickable */
function Subtitle({
  game,
  onBrand,
  className,
}: {
  game: GameEntry;
  onBrand: (b: string) => void;
  className?: string;
}) {
  const y = game.meta?.release?.slice(0, 4);
  if (!brandOf(game) && !y) return null;
  return (
    <div className={cn("truncate text-muted-foreground", className)}>
      <BrandLink game={game} onBrand={onBrand} />
      {brandOf(game) && y && " · "}
      {y}
    </div>
  );
}

function GameCard({
  game,
  last,
  favorite,
  onPick,
  onInfo,
  onBrand,
}: {
  game: GameEntry;
  last?: number;
  favorite: boolean;
  onPick: () => void;
  onInfo: () => void;
  onBrand: (b: string) => void;
}) {
  const m = game.meta;
  return (
    <li className="group relative" data-game={game.id}>
      <button
        type="button"
        onClick={onPick}
        aria-label={`${game.title} を起動`}
        className="block w-full rounded-xl text-left outline-none focus-visible:ring-[3px] focus-visible:ring-ring/60"
      >
        <div className="relative aspect-[4/3] overflow-hidden rounded-xl border bg-card shadow-sm transition duration-200 group-hover:-translate-y-0.5 group-hover:shadow-lg group-hover:ring-2 group-hover:ring-primary/70">
          <Cover game={game} className="transition duration-300 group-hover:scale-[1.03]" />
          <div className="absolute inset-0 grid place-items-center bg-black/0 opacity-0 transition group-hover:bg-black/35 group-hover:opacity-100">
            <span className="grid size-12 place-items-center rounded-full bg-primary text-primary-foreground shadow-lg">
              <Play className="size-5 fill-current" />
            </span>
          </div>
          {favorite && (
            <span
              title="お気に入り"
              className="absolute top-2 left-2 grid size-7 place-items-center rounded-full bg-black/60 text-rose-400 backdrop-blur"
            >
              <Heart className="size-3.5 fill-current" />
            </span>
          )}
        </div>
        <div className="line-clamp-2 px-0.5 pt-2 text-sm leading-snug font-semibold">{game.title}</div>
      </button>
      <div className="px-0.5">
        <Subtitle game={game} onBrand={onBrand} className="mt-0.5 text-xs" />
        <div className="mt-1.5 flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
          {game.engine && <Badge variant="secondary">{game.engine}</Badge>}
          {!!game.unpack_issues?.length && (
            <Badge variant="destructive" title={game.unpack_issues.join("\n")}>
              一部未展開
            </Badge>
          )}
          {!!m?.score && (
            <span className="inline-flex items-center gap-0.5 font-medium text-star">
              <Star className="size-3 fill-current" />
              {m.score}
            </span>
          )}
          {last && <span>前回 {when(last)}</span>}
        </div>
      </div>
      <Button
        size="icon-sm"
        variant="secondary"
        onClick={onInfo}
        aria-label={`${game.title} の情報`}
        title="ゲーム情報"
        className="absolute top-2 right-2 rounded-full bg-black/60 text-white backdrop-blur hover:bg-primary hover:text-primary-foreground"
      >
        <Info />
      </Button>
    </li>
  );
}

/** The game last played here, large, to go on with it. */
function Continue({
  game,
  last,
  onPick,
  onInfo,
  onBrand,
}: {
  game: GameEntry;
  last: number;
  onPick: () => void;
  onInfo: () => void;
  onBrand: (b: string) => void;
}) {
  const url = coverUrl(game);
  return (
    <section
      aria-label="続きから遊ぶ"
      className="relative mb-8 overflow-hidden rounded-2xl border bg-card shadow-sm"
    >
      {url && (
        <img
          src={url}
          alt=""
          aria-hidden
          className="absolute inset-0 size-full scale-110 object-cover opacity-30 blur-2xl"
        />
      )}
      <div className="relative flex items-center gap-4 p-3 sm:gap-6 sm:p-5">
        <div className="aspect-[4/3] w-28 shrink-0 overflow-hidden rounded-lg border text-xs shadow-lg sm:w-56 sm:rounded-xl sm:text-base">
          <Cover game={game} />
        </div>
        <div className="min-w-0 flex-1">
          <p className="text-xs font-medium tracking-wide text-primary">前回のつづき・{when(last)}</p>
          <h2 className="mt-1 line-clamp-2 text-lg font-bold text-balance sm:text-2xl">{game.title}</h2>
          <Subtitle game={game} onBrand={onBrand} className="mt-1 text-sm" />
          <div className="mt-3 flex flex-wrap gap-2 sm:mt-4">
            <Button className="sm:h-10 sm:px-6" onClick={onPick}>
              <Play className="fill-current" /> つづきから遊ぶ
            </Button>
            <Button variant="outline" className="sm:h-10" onClick={onInfo} aria-label="詳細">
              <Info /> <span className="max-sm:hidden">詳細</span>
            </Button>
          </div>
        </div>
      </div>
    </section>
  );
}

function Section({ title, icon, children }: { title: string; icon: ReactNode; children: ReactNode }) {
  return (
    <section className="grid gap-2.5 rounded-xl border bg-card/60 p-3.5">
      <h3 className="flex items-center gap-2 text-sm font-semibold [&_svg]:size-4 [&_svg]:text-muted-foreground">
        {icon}
        {title}
      </h3>
      {children}
    </section>
  );
}

const mb = (b: number) => `${Math.max(1, Math.round(b / 2 ** 20))} MB`;

/** The game's data on this device: save it beforehand (for a weak mobile link), or remove it. */
function Offline({ game }: { game: GameEntry }) {
  const [state, setState] = useState<OfflineState | null | undefined>();
  const [saving, setSaving] = useState<{ have: number; total: number; stop: AbortController } | null>(null);
  const [error, setError] = useState("");
  const refresh = () =>
    void offlineState(game).then(setState, (e: unknown) => {
      setState(null);
      setError(String(e instanceof Error ? e.message : e));
    });
  useEffect(refresh, [game]);
  if (state === null && !error) return null;
  if (!state)
    return error ? (
      <p className="text-sm text-muted-foreground">保存の状態を読めませんでした（{error}）</p>
    ) : null;
  const save = () => {
    const stop = new AbortController();
    setError("");
    setSaving({ have: state.have, total: state.total, stop });
    saveOffline(game, (have, total) => setSaving({ have, total, stop }), stop.signal)
      .catch(
        (e: unknown) =>
          stop.signal.aborted ||
          setError(`保存できませんでした: ${String(e instanceof Error ? e.message : e)}`),
      )
      .finally(() => {
        setSaving(null);
        refresh();
      });
  };
  const all = state.complete;
  const pct = Math.floor((state.have / Math.max(1, state.total)) * 100);
  return (
    <div className="grid gap-2 text-sm">
      <div className="flex items-center gap-2">
        <HardDrive className="size-4 shrink-0 text-muted-foreground" />
        <span className="flex-1">
          {saving
            ? `端末に保存中 ${Math.floor((saving.have / Math.max(1, saving.total)) * 100)}%`
            : all
              ? `端末に保存済み（約 ${mb(state.bytes)}）`
              : state.status === "stopped"
                ? `端末への保存を中止しました（${pct}%、全体 約 ${mb(state.bytes)}）。続きから保存できます`
                : state.status === "failed"
                  ? `端末への保存が途中で失敗しました（${pct}%、全体 約 ${mb(state.bytes)}）`
                  : state.status === "done"
                    ? `端末に保存したデータの一部が消えています（${pct}%、全体 約 ${mb(state.bytes)}）`
                    : state.have
                      ? `一部を端末に保存済み（${pct}%、全体 約 ${mb(state.bytes)}）`
                      : `端末に保存すると、モバイル回線でも読み込みで止まりません（約 ${mb(state.bytes)}）`}
        </span>
      </div>
      {error && <p className="text-destructive">{error}</p>}
      <div className="flex gap-2">
        {saving ? (
          <Button size="sm" variant="outline" onClick={() => saving.stop.abort()}>
            <X /> 中止
          </Button>
        ) : (
          !all && (
            <Button size="sm" variant="outline" onClick={save}>
              <Download /> 端末に保存
            </Button>
          )
        )}
        {!saving && (state.have > 0 || state.status !== "none") && (
          <Button size="sm" variant="ghost" onClick={() => void dropOffline(game).finally(refresh)}>
            <Trash2 /> 端末から削除
          </Button>
        )}
      </div>
    </div>
  );
}

/** 批評空間 / VNDB の作品ページの URL から、サーバーがこのゲームの情報を取り直す。 */
function MetaSource({ game, onUpdated }: { game: GameEntry; onUpdated: () => Promise<void> }) {
  const page = game.meta?.links?.find((l) => looksLikeGamePage(l.url))?.url ?? "";
  const [url, setUrl] = useState("");
  const [status, setStatus] = useState<MetaStatus | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const stop = useRef<AbortController | null>(null);
  useEffect(() => {
    const ac = new AbortController();
    stop.current = ac;
    void metaStatus(game.id).then(
      (s) => {
        if (ac.signal.aborted) return;
        setStatus(s);
        setUrl((u) => u || s?.url || page);
        if (s?.state === "pending") void follow(ac);
      },
      () => setUrl((u) => u || page),
    );
    return () => ac.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [game.id]);
  async function follow(ac: AbortController) {
    setBusy(true);
    try {
      const end = await untilSettled(game.id, (s) => !ac.signal.aborted && setStatus(s), ac.signal);
      if (end?.state === "ok" && !ac.signal.aborted) await onUpdated();
    } catch (e) {
      if (!ac.signal.aborted) setError(String(e instanceof Error ? e.message : e));
    } finally {
      if (!ac.signal.aborted) setBusy(false);
    }
  }
  const valid = looksLikeGamePage(url);
  const submit = async (page = url.trim()) => {
    setError("");
    setBusy(true);
    try {
      setStatus(await fetchMeta(game.id, page));
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
      setBusy(false);
      return;
    }
    if (stop.current) void follow(stop.current);
  };
  const said = error || describe(status);
  return (
    <form
      className="grid gap-2"
      onSubmit={(e) => {
        e.preventDefault();
        if (valid && !busy) void submit();
      }}
    >
      <p className="text-xs text-muted-foreground">
        批評空間か VNDB の作品ページの URL
        を貼ると、サーバーがその情報（タイトル・ブランド・発売日・タグ・あらすじ・画像）を取ってきて置き換えます。
      </p>
      <div className="flex gap-2">
        <Input
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="https://erogamescape.dyndns.org/~ap2/ero/toukei_kaiseki/game.php?game=…"
          aria-label="批評空間・VNDB の作品ページの URL"
          aria-invalid={!!url.trim() && !valid}
          inputMode="url"
          className="min-w-0 flex-1"
          data-testid="meta-url"
        />
        <Button type="submit" size="sm" className="h-9" disabled={!valid || busy} data-testid="meta-fetch">
          <RefreshCw className={cn(busy && "animate-spin")} /> {busy ? "取得中" : "取得"}
        </Button>
      </div>
      {url.trim() && !valid && (
        <p className="text-xs text-destructive">
          批評空間（game.php?game=番号）か VNDB（vndb.org/v番号）の作品ページの URL を入れてください
        </p>
      )}
      <Candidates
        game={game}
        disabled={busy}
        onPick={(c) => {
          setUrl(egsPage(c.id));
          void submit(`egs:${c.id}`);
        }}
      />
      {said && (
        <p
          role="status"
          data-testid="meta-status"
          className={cn(
            "text-xs",
            error || status?.state === "error" ? "text-destructive" : "text-muted-foreground",
          )}
        >
          {said}
        </p>
      )}
    </form>
  );
}

/** 批評空間の全作品から、このゲームの名前（タイトル・フォルダ・exe）に近いものを出して選ばせる。 */
function Candidates({
  game,
  disabled,
  onPick,
}: {
  game: GameEntry;
  disabled: boolean;
  onPick: (c: Candidate) => void;
}) {
  const [list, setList] = useState<Candidate[] | null>(null);
  const [q, setQ] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const look = async () => {
    setLoading(true);
    setError("");
    try {
      setList(await candidates(game.id, q));
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setLoading(false);
    }
  };
  return (
    <div className="grid gap-2">
      <div className="flex gap-2">
        <Input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              void look();
            }
          }}
          placeholder="作品名で探す（空ならタイトルとフォルダ名・exe名から）"
          aria-label="批評空間の作品を名前で探す"
          className="min-w-0 flex-1"
          data-testid="meta-search"
        />
        <Button
          type="button"
          size="sm"
          variant="outline"
          className="h-9"
          disabled={loading}
          onClick={() => void look()}
          data-testid="meta-candidates"
        >
          <Search className={cn(loading && "animate-pulse")} /> {loading ? "探しています" : "候補を探す"}
        </Button>
      </div>
      {loading && !list && (
        <p className="text-xs text-muted-foreground">
          初回は批評空間の作品一覧を取ってくるので少しかかります
        </p>
      )}
      {error && <p className="text-xs text-destructive">{error}</p>}
      {list && !list.length && (
        <p className="text-xs text-muted-foreground">近い作品が見つかりませんでした</p>
      )}
      {!!list?.length && (
        <ul className="grid gap-1" aria-label="批評空間の候補" data-testid="meta-candidate-list">
          {list.map((c) => (
            <li key={c.id} className="flex items-center gap-2 rounded-md border px-2 py-1.5 text-sm">
              <div className="min-w-0 flex-1">
                <a
                  href={egsPage(c.id)}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="break-all hover:underline"
                >
                  {c.name}
                </a>
                <span className="ml-2 text-xs text-muted-foreground tabular-nums">
                  {Math.round(c.score * 100)}%{c.votes ? ` · ${c.votes} 票` : ""}
                </span>
                {c.best && (
                  <Badge variant="secondary" className="ml-2 rounded-full font-normal">
                    自動で選ばれる作品
                  </Badge>
                )}
              </div>
              <Button
                type="button"
                size="sm"
                variant="secondary"
                disabled={disabled}
                onClick={() => onPick(c)}
              >
                これにする
              </Button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** The sidebar's 詳細 tab: a game's details and records. */
function GameInfo({
  game,
  onPick,
  onFavorite,
  onUpdated,
  onBrand,
}: {
  game: GameEntry;
  onPick: () => void;
  onFavorite: (on: boolean) => void;
  onBrand: (b: string) => void;
  onUpdated: () => Promise<void>;
}) {
  const m = game.meta ?? { source: "", title: game.title };
  const url = coverUrl(game);
  const record = useProfile(game.id, onFavorite);
  const favorite = !!record.profile?.favorite;
  const facts: [string, ReactNode][] = [
    ["ブランド", brandOf(game) && <BrandLink game={game} onBrand={onBrand} className="font-medium" />],
    ["発売日", m.release],
    ["シリーズ", m.series],
    [
      m.source === "vndb" ? "VNDB 評価" : "批評空間 中央値",
      m.score ? `${m.score}${m.votes ? `（${m.votes} 票）` : ""}` : undefined,
    ],
    ["エンジン", game.engine],
  ];
  return (
    <div data-testid="game-info">
      {url && (
        <div className="relative bg-black">
          <img src={url} alt="" className="max-h-80 w-full object-contain" />
          <div className="pointer-events-none absolute inset-x-0 bottom-0 h-16 bg-gradient-to-t from-background to-transparent" />
        </div>
      )}
      <div className="grid gap-4 p-5">
        <div className="flex flex-col gap-1">
          <h2 className="text-xl leading-snug font-semibold">{m.title || game.title}</h2>
          {m.furigana && m.furigana !== (m.title || game.title) && (
            <p className="text-sm text-muted-foreground">{m.furigana}</p>
          )}
        </div>
        <div className="flex gap-2">
          <Button size="lg" className="flex-1" onClick={onPick}>
            <Play className="fill-current" /> 起動
          </Button>
          <Button
            size="lg"
            variant="outline"
            aria-pressed={favorite}
            disabled={!record.profile || record.busy}
            onClick={() => void record.toggleFavorite()}
            title={favorite ? "お気に入りから外す" : "お気に入りに追加"}
          >
            <Heart className={cn(favorite && "fill-current text-rose-400")} />
            <span className="max-sm:sr-only">{favorite ? "お気に入り" : "お気に入りに追加"}</span>
          </Button>
        </div>
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-sm">
          {facts
            .filter(([, v]) => v)
            .map(([k, v]) => (
              <div key={k} className="contents">
                <dt className="text-muted-foreground">{k}</dt>
                <dd>{v}</dd>
              </div>
            ))}
        </dl>
        {!!m.tags?.length && (
          <div className="flex flex-wrap gap-1.5">
            {m.tags.map((t) => (
              <Badge key={t} variant="secondary" className="rounded-full font-normal">
                {t}
              </Badge>
            ))}
          </div>
        )}
        {m.description && (
          <p className="text-sm leading-relaxed whitespace-pre-line text-muted-foreground">{m.description}</p>
        )}
        {!!m.links?.length && (
          <div className="flex flex-wrap gap-2">
            {m.links.map((l) => (
              <Button key={l.url} asChild size="sm" variant="outline">
                <a href={l.url} target="_blank" rel="noopener noreferrer">
                  {l.label} <ExternalLink className="size-3" />
                </a>
              </Button>
            ))}
          </div>
        )}
        {!!game.unpack_issues?.length && (
          <div className="grid gap-1.5 rounded-lg border border-destructive/40 p-3 text-sm">
            <p className="flex items-center gap-2 font-medium text-destructive">
              <TriangleAlert className="size-4 shrink-0" />
              取り込み時に展開できなかったものがあります（動かない場面があるかもしれません）
            </p>
            <ul className="grid gap-1 text-xs break-all text-muted-foreground">
              {game.unpack_issues.map((t) => (
                <li key={t}>{t}</li>
              ))}
            </ul>
          </div>
        )}
        <Section title="作品情報を取り直す" icon={<RefreshCw />}>
          <MetaSource key={game.id} game={game} onUpdated={onUpdated} />
        </Section>
        <Section title="メモ" icon={<NotebookPen />}>
          <Note state={record} />
        </Section>
        <Section title="端末に保存" icon={<HardDrive />}>
          <Offline game={game} />
        </Section>
        <Section title="バックアップ" icon={<Download />}>
          <p className="text-xs text-muted-foreground">
            セーブデータとメモを 1 つのファイルにして持ち運べます。
          </p>
          <Backup game={game.id} state={record} />
        </Section>
      </div>
    </div>
  );
}

const VIEW = "ippai-library-view";
const loadView = (): ListView => {
  try {
    return parseView(localStorage.getItem(VIEW));
  } catch {
    return parseView(null);
  }
};
const saveView = (v: ListView) => {
  try {
    localStorage.setItem(VIEW, JSON.stringify(v));
  } catch {
    // (storage blocked: the choice lasts for this page)
  }
};

function ViewSelect({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: string;
  options: readonly { key: string; label: string }[];
  onChange: (v: string) => void;
}) {
  return (
    <label className="flex min-w-0 items-center gap-1.5 text-xs whitespace-nowrap text-muted-foreground">
      {label}
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="h-8 min-w-0 flex-1 rounded-md border bg-background px-2 text-sm text-foreground shadow-xs outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
      >
        {options.map((o) => (
          <option key={o.key} value={o.key}>
            {o.label}
          </option>
        ))}
      </select>
    </label>
  );
}

export function Library({ games: listed, onPick }: { games: GameEntry[]; onPick: (g: GameEntry) => void }) {
  const [games, setGames] = useState(listed);
  const [favorites, setFavorites] = useState<string[]>([]);
  const [favoriteError, setFavoriteError] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  // the sidebar's tab (null: closed)
  const [side, setSideState] = useState<SideTab | null>(null);
  // the tab the sidebar opens on again (the last one shown)
  const [lastTab, setLastTab] = useState<SideTab>("settings");
  const setSide = (t: SideTab | null) => {
    setSideState(t);
    if (t) setLastTab(t);
  };
  const sideButton = useRef<HTMLButtonElement>(null);
  const [query, setQuery] = useState("");
  // the game the 詳細 tab shows (kept while another tab is open)
  const [info, setInfo] = useState<GameEntry | null>(null);
  const showInfo = (g: GameEntry) => {
    setInfo(g);
    setSide("info");
  };
  const [build, setBuild] = useState("");
  useEffect(() => {
    void fetchBuild().then(setBuild);
  }, []);
  useEffect(() => {
    void api<string[]>("favorites")
      .then((ids) => {
        setFavorites(ids);
        setFavoriteError("");
      })
      // (offline: the list shows no favorites, without an error)
      .catch((e: unknown) => setFavoriteError(navigator.onLine ? String(e) : ""));
  }, []);
  const app = useApp();
  const last = useMemo(played, []);
  const [view, setViewState] = useState<ListView>(loadView);
  const setView = (v: Partial<ListView>) =>
    setViewState((old) => {
      const next = { ...old, ...v };
      saveView(next);
      return next;
    });
  const byRecent = useMemo(() => sortGames(games, last), [games, last]);
  const ordered = useMemo(() => orderGames(games, view.sort, last), [games, view.sort, last]);
  // a brand's name clicked: only its games (cleared with the chip's ×)
  const [brand, setBrand] = useState<string | null>(null);
  const pickBrand = (b: string) => {
    setBrand(b);
    document.querySelector("main")?.scrollTo({ top: 0 });
  };
  const shown = visibleGames(ordered, query, filter, favorites, brand);
  const groups = groupGames(shown, view.group);
  const recent = byRecent[0] && last[byRecent[0].id] ? byRecent[0] : null;
  const favoriteCount = games.filter((g) => favorites.includes(g.id)).length;
  const setFavorite = (id: string, on: boolean) =>
    setFavorites((f) => (on ? [...new Set([...f, id])] : f.filter((x) => x !== id)));
  return (
    <main className={cn("fixed inset-0 z-40 overflow-y-auto bg-background", side && "lg:pr-96")}>
      <header className="sticky top-0 z-10 border-b bg-background/80 backdrop-blur-md">
        <div className="mx-auto flex h-14 max-w-7xl items-center gap-3 px-4">
          <a
            href="./"
            className="flex shrink-0 items-center gap-2 rounded-md outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
          >
            <Logo />
            <h1 className="text-lg font-bold tracking-tight">ippai</h1>
            {build && (
              <span className="text-[10px] text-muted-foreground tabular-nums sm:text-xs" data-testid="build">
                {build}
              </span>
            )}
          </a>
          {games.length > 0 && (
            <div className="relative mx-auto w-full max-w-md min-w-0">
              <Search className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                type="search"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="ゲームを探す"
                aria-label="タイトル・ブランド・タグで絞り込み"
                className="rounded-full pl-8"
              />
            </div>
          )}
          <div className="ml-auto flex shrink-0 items-center gap-1.5" data-header-actions>
            <Button
              ref={sideButton}
              variant={side ? "secondary" : "outline"}
              size="icon"
              onClick={() => setSide(side ? null : lastTab)}
              aria-label="サイドバー"
              title={side ? "サイドバーを閉じる" : "サイドバーを開く"}
              aria-expanded={!!side}
            >
              {side ? <PanelRightClose aria-hidden="true" /> : <PanelRightOpen aria-hidden="true" />}
            </Button>
          </div>
        </div>
      </header>
      <div className="mx-auto max-w-7xl px-4 pt-5 pb-12">
        {!app.online && (
          <p className="mb-4 flex items-center gap-2 rounded-lg border border-dashed p-3 text-sm text-muted-foreground">
            <WifiOff className="size-4 shrink-0" />
            オフラインです。「端末に保存」したゲームを遊べます。
          </p>
        )}
        {app.updated && (
          <p className="mb-4 flex items-center gap-2 rounded-lg border p-3 text-sm">
            <RefreshCw className="size-4 shrink-0 text-primary" />
            <span className="flex-1">新しいバージョンがあります。</span>
            <Button size="sm" variant="outline" onClick={() => location.reload()}>
              再読み込み
            </Button>
          </p>
        )}
        {favoriteError && (
          <p role="alert" className="mb-4 text-sm text-destructive">
            {favoriteError}
          </p>
        )}
        {!games.length ? (
          <div className="mx-auto mt-16 grid max-w-xl justify-items-center gap-3 rounded-2xl border border-dashed p-10 text-center text-sm leading-relaxed text-muted-foreground">
            <Logo className="size-14" />
            <p className="text-base font-medium text-foreground">ゲームがまだありません</p>
            <p>
              ゲームのフォルダを{" "}
              <code className="rounded bg-muted px-1.5 py-0.5">ippai add &lt;dir&gt; --exe &lt;exe&gt;</code>{" "}
              で追加すると、ここに並びます。
            </p>
          </div>
        ) : (
          <>
            {recent && !query.trim() && filter === "all" && brand === null && (
              <Continue
                game={recent}
                last={last[recent.id]}
                onPick={() => onPick(recent)}
                onInfo={() => showInfo(recent)}
                onBrand={pickBrand}
              />
            )}
            <div className="mb-4 flex flex-wrap items-center gap-3">
              <h2 className="text-base font-semibold">ライブラリ</h2>
              <Tabs value={filter} onValueChange={(v) => setFilter(v as Filter)}>
                <TabsList aria-label="表示するゲーム">
                  <TabsTrigger value="all">
                    すべて <span className="text-xs text-muted-foreground tabular-nums">{games.length}</span>
                  </TabsTrigger>
                  <TabsTrigger value="favorites">
                    <Heart className="size-3.5" /> お気に入り{" "}
                    <span className="text-xs text-muted-foreground tabular-nums">{favoriteCount}</span>
                  </TabsTrigger>
                </TabsList>
              </Tabs>
              {brand !== null && (
                <Badge
                  variant="secondary"
                  className="h-8 gap-1 rounded-full pr-1 pl-3 text-sm font-normal"
                  data-testid="brand-filter"
                >
                  ブランド: <span className="font-medium">{brand}</span>
                  <button
                    type="button"
                    onClick={() => setBrand(null)}
                    aria-label="ブランドの絞り込みを解除"
                    className="grid size-6 place-items-center rounded-full hover:bg-background/60 focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none"
                  >
                    <X className="size-3.5" />
                  </button>
                </Badge>
              )}
              <div className="grid w-full grid-cols-2 gap-2 sm:ml-auto sm:flex sm:w-auto">
                <ViewSelect
                  label="並び順"
                  value={view.sort}
                  options={SORTS}
                  onChange={(sort) => setView({ sort: sort as SortKey })}
                />
                <ViewSelect
                  label="区分け"
                  value={view.group}
                  options={GROUPS}
                  onChange={(group) => setView({ group: group as GroupKey })}
                />
              </div>
            </div>
            {!shown.length ? (
              <div className="mt-12 grid justify-items-center gap-3 text-center text-sm text-muted-foreground">
                {query.trim() || brand !== null ? (
                  <>
                    <p>
                      {[query.trim() && `「${query.trim()}」`, brand !== null && `ブランド「${brand}」`]
                        .filter(Boolean)
                        .join("・")}
                      に合うゲームはありません
                    </p>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => {
                        setQuery("");
                        setBrand(null);
                      }}
                    >
                      <X /> 検索をクリア
                    </Button>
                  </>
                ) : (
                  <p>お気に入りはまだありません。ゲームの詳細から ♡ で追加できます。</p>
                )}
              </div>
            ) : (
              <div className="grid gap-8">
                {groups.map((group) => (
                  <section key={group.key} aria-label={group.label || "ゲーム"} data-group={group.key}>
                    {group.label && (
                      <h3 className="mb-3 flex items-baseline gap-2 border-b pb-1.5 text-sm font-semibold">
                        {group.label}
                        <span className="text-xs font-normal text-muted-foreground tabular-nums">
                          {group.games.length}
                        </span>
                      </h3>
                    )}
                    <ul className="grid grid-cols-2 gap-x-3 gap-y-6 sm:grid-cols-[repeat(auto-fill,minmax(200px,1fr))] sm:gap-x-4">
                      {group.games.map((g) => (
                        <GameCard
                          key={g.id}
                          game={g}
                          last={last[g.id]}
                          favorite={favorites.includes(g.id)}
                          onPick={() => onPick(g)}
                          onInfo={() => showInfo(g)}
                          onBrand={pickBrand}
                        />
                      ))}
                    </ul>
                  </section>
                ))}
              </div>
            )}
          </>
        )}
      </div>
      {side && (
        <Sidebar
          tab={side}
          onTab={setSide}
          games={games}
          build={build}
          game={info?.title ?? null}
          onClose={() => setSide(null)}
          returnFocus={() => sideButton.current?.focus()}
        >
          {info && (
            <GameInfo
              key={info.id}
              game={info}
              onPick={() => onPick(info)}
              onFavorite={(on) => setFavorite(info.id, on)}
              onBrand={pickBrand}
              onUpdated={async () => {
                // (the list again: the server's entry now carries what it fetched)
                const fresh = await fetchGames();
                setGames(fresh);
                setInfo((open) => (open ? (fresh.find((g) => g.id === open.id) ?? open) : open));
              }}
            />
          )}
        </Sidebar>
      )}
    </main>
  );
}

/** Shows the list over the page; `pick` runs in the click that chose a game. */
export function showLibrary(games: GameEntry[], pick: (g: GameEntry) => void): void {
  renderShell(<Library games={games} onPick={pick} />);
}
