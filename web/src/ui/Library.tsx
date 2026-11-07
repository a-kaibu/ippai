import { api } from "../library-data";
import { Personal, Statistics } from "./Personal";
// The game list (play.html without ?run=): cards with cover, brand, year and score from
// `ippai meta`, a filter, and a dialog with the game's details. Picking a game starts it from
// that click (audio and fullscreen need the user gesture).

import { useEffect, useMemo, useState } from "react";
import {
  Bug,
  Download,
  ExternalLink,
  Gamepad2,
  HardDrive,
  Info,
  MonitorDown,
  Play,
  RefreshCw,
  ScrollText,
  Search,
  Star,
  Trash2,
  WifiOff,
  X,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { coverUrl, played, type GameEntry } from "../launcher";
import { dropOffline, offlineState, saveOffline, type OfflineState } from "../offline";
import { useApp } from "../pwa";
import { renderShell } from "./Shell";
import { setDebug, useUi } from "./store";

function when(t: number): string {
  const d = new Date(t);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getMonth() + 1}/${d.getDate()} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

function Cover({ game, className }: { game: GameEntry; className?: string }) {
  const url = coverUrl(game);
  return url ? (
    <img src={url} alt="" loading="lazy" className={cn("size-full object-cover", className)} />
  ) : (
    <div className="grid size-full place-items-center bg-gradient-to-br from-secondary to-background p-3 text-center font-semibold text-balance text-muted-foreground">
      {game.title}
    </div>
  );
}

function GameCard({
  game,
  last,
  onPick,
  onInfo,
}: {
  game: GameEntry;
  last?: number;
  onPick: () => void;
  onInfo: () => void;
}) {
  const m = game.meta;
  const sub = [m?.brand, m?.release?.slice(0, 4)].filter(Boolean).join(" · ");
  return (
    <li className="group relative">
      <button
        type="button"
        onClick={onPick}
        className="block w-full rounded-xl text-left outline-none focus-visible:ring-[3px] focus-visible:ring-ring/60"
      >
        <div className="relative aspect-[4/3] overflow-hidden rounded-xl border bg-card shadow-sm transition duration-200 group-hover:-translate-y-0.5 group-hover:shadow-lg group-hover:ring-2 group-hover:ring-primary/70">
          <Cover game={game} className="transition duration-300 group-hover:scale-[1.03]" />
          <div className="absolute inset-0 grid place-items-center bg-black/0 opacity-0 transition group-hover:bg-black/35 group-hover:opacity-100">
            <span className="grid size-12 place-items-center rounded-full bg-primary text-primary-foreground shadow-lg">
              <Play className="size-5 fill-current" />
            </span>
          </div>
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
      {
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
      }
    </li>
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
  if (!state) return error ? <p className="text-sm text-muted-foreground">{error}</p> : null;
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
  const all = state.have >= state.total;
  return (
    <div className="grid gap-2 rounded-lg border p-3 text-sm">
      <div className="flex items-center gap-2">
        <HardDrive className="size-4 shrink-0 text-muted-foreground" />
        <span className="flex-1">
          {saving
            ? `端末に保存中 ${Math.floor((saving.have / Math.max(1, saving.total)) * 100)}%`
            : all
              ? `端末に保存済み（約 ${mb(state.bytes)}）`
              : state.have
                ? `一部を端末に保存済み（${Math.floor((state.have / state.total) * 100)}%、全体 約 ${mb(state.bytes)}）`
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
        {!saving && state.have > 0 && (
          <Button size="sm" variant="ghost" onClick={() => void dropOffline(game).finally(refresh)}>
            <Trash2 /> 端末から削除
          </Button>
        )}
      </div>
    </div>
  );
}

function GameInfo({ game, onPick, onClose }: { game: GameEntry; onPick: () => void; onClose: () => void }) {
  const m = game.meta ?? { source: "", title: game.title };
  const url = coverUrl(game);
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
      <DialogContent className="gap-0 p-0">
        {url && <img src={url} alt="" className="max-h-80 w-full bg-black object-contain" />}
        <div className="grid gap-4 p-5">
          <DialogHeader>
            <DialogTitle className="pr-8">{m.title || game.title}</DialogTitle>
            <DialogDescription>{m.furigana || game.title}</DialogDescription>
          </DialogHeader>
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
              <p className="font-medium text-destructive">
                取り込み時に展開できなかったものがあります（動かない場面があるかもしれません）
              </p>
              <ul className="grid gap-1 text-xs break-all text-muted-foreground">
                {game.unpack_issues.map((t) => (
                  <li key={t}>{t}</li>
                ))}
              </ul>
            </div>
          )}
          <Personal game={game.id} />
          <Offline game={game} />
          <Button size="lg" className="w-full" onClick={onPick}>
            <Play className="fill-current" /> 起動
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

export function Library({ games, onPick }: { games: GameEntry[]; onPick: (g: GameEntry) => void }) {
  const [favorites, setFavorites] = useState<string[]>([]);
  const [onlyFavorites, setOnlyFavorites] = useState(false);
  const [favoriteError, setFavoriteError] = useState("");
  const [stats, setStats] = useState(false);
  const [query, setQuery] = useState("");
  const [info, setInfo] = useState<GameEntry | null>(null);
  useEffect(() => {
    if (info) return;
    void api<string[]>("favorites")
      .then((ids) => {
        setFavorites(ids);
        setFavoriteError("");
      })
      // (offline: the list shows no favorites, without an error)
      .catch((e: unknown) => setFavoriteError(navigator.onLine ? String(e) : ""));
  }, [info]);
  const { debug } = useUi();
  const app = useApp();
  const last = useMemo(played, []);
  const sorted = useMemo(
    () =>
      [...games].sort(
        (a, b) => (last[b.id] ?? 0) - (last[a.id] ?? 0) || a.title.localeCompare(b.title, "ja"),
      ),
    [games, last],
  );
  const q = query.trim().toLowerCase();
  const searched = q
    ? sorted.filter((g) => {
        const m = g.meta;
        const key = [g.title, g.engine, g.id, m?.title, m?.furigana, m?.brand, m?.series, ...(m?.tags ?? [])];
        return key.filter(Boolean).join(" ").toLowerCase().includes(q);
      })
    : sorted;
  const shown = onlyFavorites ? searched.filter((g) => favorites.includes(g.id)) : searched;
  return (
    <main className="fixed inset-0 z-40 overflow-y-auto bg-background">
      <header className="sticky top-0 z-10 border-b bg-background/80 backdrop-blur">
        <div className="mx-auto flex max-w-7xl items-center gap-3 px-4 py-3">
          <Button variant="outline" onClick={() => setStats(true)}>
            統計
          </Button>
          <Gamepad2 className="size-5 shrink-0 text-primary" />
          <h1 className="text-lg font-bold tracking-tight">vnvm</h1>
          <Badge variant="secondary">{games.length} 本</Badge>
          <div className="relative ml-auto w-full max-w-72">
            {games.length > 6 && (
              <>
                <Search className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
                <Input
                  type="search"
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  placeholder="タイトル・ブランド・タグ"
                  aria-label="タイトル・ブランド・タグで絞り込み"
                  className="pl-8"
                />
              </>
            )}
          </div>
          <label
            className="flex shrink-0 cursor-pointer items-center gap-1.5 text-sm text-muted-foreground"
            title="プレイ中に通信量・読み込み待ち・メモリ・fpsを表示（Ctrl+Shift+Dでも切り替え）"
          >
            <Switch checked={debug} onCheckedChange={setDebug} aria-label="デバッグモード" />
            <Bug className="size-4 sm:hidden" />
            <span className="max-sm:hidden">デバッグ</span>
          </label>
          {app.install && (
            <Button
              variant="outline"
              size="sm"
              onClick={app.install}
              title="ホーム画面・デスクトップにアプリとして追加"
            >
              <MonitorDown /> <span className="max-sm:hidden">アプリにする</span>
            </Button>
          )}
          <Button asChild variant="ghost" size="sm">
            <a href="logs.html">
              <ScrollText /> <span className="max-sm:hidden">ログ</span>
            </a>
          </Button>
        </div>
      </header>
      <div className="mx-auto max-w-7xl px-4 pt-5 pb-10">
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
        <label className="mb-4 flex items-center gap-2">
          <input
            type="checkbox"
            checked={onlyFavorites}
            onChange={(e) => setOnlyFavorites(e.target.checked)}
          />
          お気に入りのみ
        </label>
        {favoriteError && <p role="alert">{favoriteError}</p>}
        {!games.length ? (
          <div className="mx-auto mt-16 max-w-xl rounded-xl border border-dashed p-8 text-center text-sm leading-relaxed text-muted-foreground">
            <p className="mb-3 text-base font-medium text-foreground">ゲームがまだありません</p>
            ゲームのフォルダを{" "}
            <code className="rounded bg-muted px-1.5 py-0.5">ippai pack &lt;dir&gt; &lt;tree&gt;</code>{" "}
            で入れて、
            <code className="rounded bg-muted px-1.5 py-0.5">
              ippai add-game &lt;id&gt; --title ... --tree &lt;tree&gt; --exe &lt;exe&gt;
            </code>{" "}
            で一覧に追加します。
          </div>
        ) : !shown.length ? (
          <p className="mt-16 text-center text-sm text-muted-foreground">
            「{query}」に合うゲームはありません
          </p>
        ) : (
          <ul className="grid grid-cols-2 gap-x-3 gap-y-5 sm:grid-cols-[repeat(auto-fill,minmax(200px,1fr))] sm:gap-x-4">
            {shown.map((g) => (
              <GameCard
                key={g.id}
                game={g}
                last={last[g.id]}
                onPick={() => onPick(g)}
                onInfo={() => setInfo(g)}
              />
            ))}
          </ul>
        )}
      </div>
      {stats && (
        <Dialog open onOpenChange={setStats}>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>プレイ統計</DialogTitle>
              <DialogDescription>ゲーム別のプレイ時間</DialogDescription>
            </DialogHeader>
            <Statistics games={games} />
          </DialogContent>
        </Dialog>
      )}
      {info && <GameInfo game={info} onClose={() => setInfo(null)} onPick={() => onPick(info)} />}
    </main>
  );
}

/** Shows the list over the page; `pick` runs in the click that chose a game. */
export function showLibrary(games: GameEntry[], pick: (g: GameEntry) => void): void {
  renderShell(<Library games={games} onPick={pick} />);
}
