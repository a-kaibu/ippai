// The game list (play.html without ?run=): a header (name, search, 統計 at the top right), the
// game last played to continue, the cards with cover, brand, year and score from `ippai meta`,
// and a dialog with the game's details and records. Picking a game starts it from that click
// (audio and fullscreen need the user gesture).

import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  BarChart3,
  Download,
  ExternalLink,
  HardDrive,
  Heart,
  Info,
  MonitorDown,
  NotebookPen,
  Play,
  RefreshCw,
  Search,
  Settings,
  Star,
  Trash2,
  TriangleAlert,
  WifiOff,
  X,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";
import { coverUrl, fetchGames, played, type GameEntry } from "../launcher";
import {
  describe,
  fetchMeta,
  looksLikeGamePage,
  metaStatus,
  untilSettled,
  type MetaStatus,
} from "../meta-source";
import { api } from "../library-data";
import { sortGames, visibleGames, when, type Filter } from "../library-view";
import { dropOffline, offlineState, saveOffline, type OfflineState } from "../offline";
import { useApp } from "../pwa";
import { fetchBuild } from "../version";
import { Backup, Note, Statistics, useProfile } from "./Personal";
import { AccountSettings } from "./AccountSettings";
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

const subtitle = (g: GameEntry) => [g.meta?.brand, g.meta?.release?.slice(0, 4)].filter(Boolean).join(" · ");

function GameCard({
  game,
  last,
  favorite,
  onPick,
  onInfo,
}: {
  game: GameEntry;
  last?: number;
  favorite: boolean;
  onPick: () => void;
  onInfo: () => void;
}) {
  const m = game.meta;
  const sub = subtitle(game);
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
        <div className="px-0.5 pt-2">
          <div className="line-clamp-2 text-sm leading-snug font-semibold">{game.title}</div>
          {sub && <div className="mt-0.5 truncate text-xs text-muted-foreground">{sub}</div>}
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
      </button>
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
}: {
  game: GameEntry;
  last: number;
  onPick: () => void;
  onInfo: () => void;
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
          {subtitle(game) && <p className="mt-1 text-sm text-muted-foreground">{subtitle(game)}</p>}
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
  const submit = async () => {
    setError("");
    setBusy(true);
    try {
      setStatus(await fetchMeta(game.id, url.trim()));
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

function GameInfo({
  game,
  onPick,
  onClose,
  onFavorite,
  onUpdated,
}: {
  game: GameEntry;
  onPick: () => void;
  onClose: () => void;
  onFavorite: (on: boolean) => void;
  onUpdated: () => Promise<void>;
}) {
  const m = game.meta ?? { source: "", title: game.title };
  const url = coverUrl(game);
  const record = useProfile(game.id, onFavorite);
  const favorite = !!record.profile?.favorite;
  const facts: [string, string | undefined][] = [
    ["ブランド", m.brand],
    ["発売日", m.release],
    ["シリーズ", m.series],
    [
      m.source === "vndb" ? "VNDB 評価" : "批評空間 中央値",
      m.score ? `${m.score}${m.votes ? `（${m.votes} 票）` : ""}` : undefined,
    ],
    ["エンジン", game.engine],
  ];
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-2xl gap-0 p-0">
        {url && (
          <div className="relative bg-black">
            <img src={url} alt="" className="max-h-80 w-full object-contain" />
            <div className="pointer-events-none absolute inset-x-0 bottom-0 h-16 bg-gradient-to-t from-background to-transparent" />
          </div>
        )}
        <div className="grid gap-4 p-5">
          <DialogHeader>
            <DialogTitle className="pr-8 text-xl">{m.title || game.title}</DialogTitle>
            {m.furigana && m.furigana !== (m.title || game.title) ? (
              <DialogDescription>{m.furigana}</DialogDescription>
            ) : (
              <DialogDescription className="sr-only">ゲームの情報</DialogDescription>
            )}
          </DialogHeader>
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
            <p className="text-sm leading-relaxed whitespace-pre-line text-muted-foreground">
              {m.description}
            </p>
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
            <MetaSource game={game} onUpdated={onUpdated} />
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
      </DialogContent>
    </Dialog>
  );
}

export function Library({ games: listed, onPick }: { games: GameEntry[]; onPick: (g: GameEntry) => void }) {
  const [games, setGames] = useState(listed);
  const [favorites, setFavorites] = useState<string[]>([]);
  const [favoriteError, setFavoriteError] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [stats, setStats] = useState(false);
  const [settings, setSettings] = useState(false);
  const settingsButton = useRef<HTMLButtonElement>(null);
  const [query, setQuery] = useState("");
  const [info, setInfo] = useState<GameEntry | null>(null);
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
  const sorted = useMemo(() => sortGames(games, last), [games, last]);
  const shown = visibleGames(sorted, query, filter, favorites);
  const recent = sorted[0] && last[sorted[0].id] ? sorted[0] : null;
  const favoriteCount = games.filter((g) => favorites.includes(g.id)).length;
  const setFavorite = (id: string, on: boolean) =>
    setFavorites((f) => (on ? [...new Set([...f, id])] : f.filter((x) => x !== id)));
  return (
    <main className="fixed inset-0 z-40 overflow-y-auto bg-background">
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
              ref={settingsButton}
              variant="outline"
              size="sm"
              onClick={() => setSettings(true)}
              aria-label="設定"
            >
              <Settings aria-hidden="true" /> <span className="max-sm:hidden">設定</span>
            </Button>
            {app.install && (
              <Button
                variant="ghost"
                size="sm"
                onClick={app.install}
                title="ホーム画面・デスクトップにアプリとして追加"
              >
                <MonitorDown /> <span className="max-md:hidden">アプリにする</span>
              </Button>
            )}
            <Button
              variant="outline"
              size="sm"
              onClick={() => setStats(true)}
              aria-label="統計"
              title="プレイ統計"
            >
              <BarChart3 /> <span className="max-sm:hidden">統計</span>
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
            {recent && !query.trim() && filter === "all" && (
              <Continue
                game={recent}
                last={last[recent.id]}
                onPick={() => onPick(recent)}
                onInfo={() => setInfo(recent)}
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
            </div>
            {!shown.length ? (
              <div className="mt-12 grid justify-items-center gap-3 text-center text-sm text-muted-foreground">
                {query.trim() ? (
                  <>
                    <p>「{query.trim()}」に合うゲームはありません</p>
                    <Button size="sm" variant="outline" onClick={() => setQuery("")}>
                      <X /> 検索をクリア
                    </Button>
                  </>
                ) : (
                  <p>お気に入りはまだありません。ゲームの詳細から ♡ で追加できます。</p>
                )}
              </div>
            ) : (
              <ul className="grid grid-cols-2 gap-x-3 gap-y-6 sm:grid-cols-[repeat(auto-fill,minmax(200px,1fr))] sm:gap-x-4">
                {shown.map((g) => (
                  <GameCard
                    key={g.id}
                    game={g}
                    last={last[g.id]}
                    favorite={favorites.includes(g.id)}
                    onPick={() => onPick(g)}
                    onInfo={() => setInfo(g)}
                  />
                ))}
              </ul>
            )}
          </>
        )}
      </div>
      {settings && (
        <AccountSettings
          onClose={() => setSettings(false)}
          returnFocus={() => settingsButton.current?.focus()}
        />
      )}
      {stats && (
        <Dialog open onOpenChange={setStats}>
          <DialogContent className="max-w-xl gap-4 p-5">
            <DialogHeader>
              <DialogTitle className="flex items-center gap-2">
                <BarChart3 className="size-5 text-primary" /> プレイ統計
              </DialogTitle>
              <DialogDescription>ゲーム別のプレイ時間</DialogDescription>
            </DialogHeader>
            <Statistics games={games} />
          </DialogContent>
        </Dialog>
      )}
      {info && (
        <GameInfo
          game={info}
          onClose={() => setInfo(null)}
          onPick={() => onPick(info)}
          onFavorite={(on) => setFavorite(info.id, on)}
          onUpdated={async () => {
            // (the list again: the server's entry now carries what it fetched)
            const fresh = await fetchGames();
            setGames(fresh);
            setInfo((open) => (open ? (fresh.find((g) => g.id === open.id) ?? open) : open));
          }}
        />
      )}
    </main>
  );
}

/** Shows the list over the page; `pick` runs in the click that chose a game. */
export function showLibrary(games: GameEntry[], pick: (g: GameEntry) => void): void {
  renderShell(<Library games={games} onPick={pick} />);
}
