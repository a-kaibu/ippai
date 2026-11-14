// What is on this device and what is being fetched onto it: the sidebar's 保存 tab (the origin's
// storage in all, the chunk cache, each game saved or being saved with its progress and the file
// it fetches now) and a game's parts (its top folders, how much of each is here) under 詳細's
// 端末に保存.

import { useEffect, useState } from "react";
import { HardDrive, Loader2 } from "lucide-react";
import type { GameEntry } from "../launcher";
import { every, throttled } from "../poll";
import {
  offlineParts,
  offlineState,
  savedIds,
  savesRunning,
  storageUse,
  watchSaves,
  type OfflineState,
  type PartInfo,
  type Progress,
} from "../offline";

/** Bytes as people read them (KB to GB). */
export function size(b: number): string {
  if (b >= 2 ** 30) return `${(b / 2 ** 30).toFixed(b >= 10 * 2 ** 30 ? 0 : 1)} GB`;
  if (b >= 2 ** 20) return `${Math.round(b / 2 ** 20)} MB`;
  return `${b > 0 ? Math.max(1, Math.round(b / 2 ** 10)) : 0} KB`;
}

/** Of `all`, `part` as a whole percent (0 of 0: 100). */
export const percent = (part: number, all: number) => (all > 0 ? Math.floor((part / all) * 100) : 100);

/** A save's rate since it started (bytes/s) and the time left at that rate (s; undefined before it is known). */
export function rate(p: Progress, now = Date.now()): { perSecond: number; left?: number } {
  const s = (now - p.started) / 1000;
  const perSecond = s > 1 ? (p.haveBytes - p.startBytes) / s : 0;
  return { perSecond, left: perSecond > 0 ? (p.totalBytes - p.haveBytes) / perSecond : undefined };
}

/** "3 分" and the like. */
export function duration(s: number): string {
  if (s < 60) return `${Math.max(1, Math.round(s))} 秒`;
  if (s < 3600) return `${Math.round(s / 60)} 分`;
  return `${Math.floor(s / 3600)} 時間 ${Math.round((s % 3600) / 60)} 分`;
}

/** The saves going on in this page, kept current. */
export function useSaves(): ReadonlyMap<string, Progress> {
  // (a new Map each change: React compares snapshots)
  const [snap, setSnap] = useState(() => new Map(savesRunning()));
  // (at most 4 times a second: a fast link saves many chunks a second, each a render of the list)
  useEffect(() => {
    const update = throttled(250, () => setSnap(new Map(savesRunning())));
    const off = watchSaves(update);
    return () => {
      off();
      update.cancel();
    };
  }, []);
  return snap;
}

function Bar({ value, label }: { value: number; label: string }) {
  return (
    <div
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={value}
      className="h-1.5 w-full overflow-hidden rounded-full bg-muted"
    >
      <div className="h-full rounded-full bg-primary transition-[width]" style={{ width: `${value}%` }} />
    </div>
  );
}

/** One save going on: bytes done of all, the rate and time left, the file it fetches now. */
export function SaveProgress({ p }: { p: Progress }) {
  const r = rate(p);
  const pct = percent(p.haveBytes, p.totalBytes);
  return (
    <div className="grid gap-1.5 text-xs" aria-live="polite">
      <Bar value={pct} label="保存の進み" />
      <p className="flex flex-wrap justify-between gap-x-3 text-muted-foreground">
        <span>
          {size(p.haveBytes)} / {size(p.totalBytes)}（{pct}%、{p.have} / {p.total} 個）
        </span>
        {r.perSecond > 0 && (
          <span>
            {size(r.perSecond)}/秒{r.left !== undefined && `・残り約 ${duration(r.left)}`}
          </span>
        )}
      </p>
      {p.now && (
        <p className="truncate text-muted-foreground" title={p.now}>
          取得中: {p.now}
        </p>
      )}
    </div>
  );
}

/** A game's parts (top folders): their size and how much of each is on the device. */
export function GameParts({ game, saving }: { game: GameEntry; saving: boolean }) {
  const [parts, setParts] = useState<PartInfo[] | null | undefined>();
  useEffect(() => {
    let live = true;
    const load = () =>
      offlineParts(game).then(
        (p) => live && setParts(p),
        () => live && setParts(null),
      );
    // (while it is being saved: the parts fill as it goes)
    const stop = saving ? every(3000, load) : (void load(), () => {});
    return () => {
      live = false;
      stop();
    };
  }, [game, saving]);
  if (!parts)
    return parts === undefined ? <p className="text-xs text-muted-foreground">内訳を読み込み中…</p> : null;
  return (
    <details className="text-xs">
      <summary className="cursor-pointer text-muted-foreground">内訳（{parts.length} 項目）</summary>
      <table className="mt-2 w-full table-fixed border-collapse" aria-label="保存するデータの内訳">
        <thead className="text-left text-muted-foreground">
          <tr>
            <th className="w-[45%] py-1 font-normal">中身</th>
            <th className="py-1 text-right font-normal">ファイル</th>
            <th className="py-1 text-right font-normal">容量</th>
            <th className="py-1 text-right font-normal">端末に</th>
          </tr>
        </thead>
        <tbody>
          {parts.map((p) => (
            <tr key={p.name} className="border-t">
              <td className="truncate py-1" title={p.name}>
                {p.name}
              </td>
              <td className="py-1 text-right tabular-nums">{p.files}</td>
              <td className="py-1 text-right tabular-nums">{size(p.bytes)}</td>
              <td className="py-1 text-right tabular-nums">{percent(p.have, p.bytes)}%</td>
            </tr>
          ))}
        </tbody>
      </table>
    </details>
  );
}

type Use = Awaited<ReturnType<typeof storageUse>>;

/** The sidebar's 保存 tab. */
export function StorageOverview({ games }: { games: GameEntry[] }) {
  const saves = useSaves();
  const [use, setUse] = useState<Use | undefined>();
  const [states, setStates] = useState<Map<string, OfflineState | null>>(new Map());
  const ids = [...new Set([...savedIds(), ...saves.keys()])];
  const shown = games.filter((g) => ids.includes(g.id));
  const key = shown.map((g) => g.id).join(",");
  const busy = saves.size > 0;
  useEffect(() => {
    let live = true;
    const load = async () => {
      const u = await storageUse().catch(() => null);
      const s = new Map<string, OfflineState | null>();
      for (const g of shown) s.set(g.id, await offlineState(g).catch(() => null));
      if (live) {
        setUse(u);
        setStates(s);
      }
    };
    // (while a save runs: again 5 s after each pass ends, never one on top of another)
    const stop = busy ? every(5000, load) : (void load(), () => {});
    return () => {
      live = false;
      stop();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, busy]);
  if (use === undefined)
    return <Loader2 className="size-4 animate-spin text-muted-foreground" aria-label="読み込み中" />;
  if (use === null)
    return <p className="text-sm text-muted-foreground">このブラウザでは端末にデータを保存できません。</p>;
  const savedBytes = [...states.values()].reduce(
    (a, s) => a + (s ? (s.bytes * s.have) / Math.max(1, s.total) : 0),
    0,
  );
  return (
    <div className="grid gap-4 text-sm">
      <div className="grid gap-1.5">
        {use.quota ? (
          <>
            <Bar value={percent(use.usage ?? 0, use.quota)} label="ブラウザの保存領域の使用量" />
            <p className="text-xs text-muted-foreground">
              このサイトの保存領域: {size(use.usage ?? 0)} / {size(use.quota)}{" "}
              使用（ブラウザが決める上限。Cache Storage・OPFS など全部で）
              {use.persisted === false && "。空き容量が減るとブラウザが消すことがあります"}
            </p>
          </>
        ) : null}
        <p className="text-xs text-muted-foreground">
          ゲームのデータ: {size(use.chunks)}（{use.count} 個）。うち端末に保存した作品 約{" "}
          {size(Math.min(savedBytes, use.chunks))}、プレイ中に読んだ分・先読みした分 約{" "}
          {size(Math.max(0, use.chunks - savedBytes))}（こちらは 2 GB を超えると古いものから消えます）
        </p>
      </div>
      {shown.length === 0 ? (
        <p className="text-xs text-muted-foreground">
          端末に保存した作品はありません。作品を起動すると先に保存されます（作品の詳細の「端末に保存」からも保存できます）。
        </p>
      ) : (
        <ul className="grid gap-3" aria-label="端末に保存した作品">
          {shown.map((g) => {
            const s = states.get(g.id);
            const p = saves.get(g.id);
            return (
              <li key={g.id} className="grid gap-1.5 rounded-lg border p-3">
                <p className="flex items-center gap-2 font-medium">
                  <HardDrive className="size-4 shrink-0 text-muted-foreground" />
                  <span className="min-w-0 flex-1 truncate">{g.title}</span>
                  {s && !p && (
                    <span className="shrink-0 text-xs font-normal text-muted-foreground">
                      {size((s.bytes * s.have) / Math.max(1, s.total))} / {size(s.bytes)}
                      {s.complete ? "（保存済み）" : `（${percent(s.have, s.total)}%）`}
                    </span>
                  )}
                </p>
                {p ? <SaveProgress p={p} /> : null}
                <GameParts game={g} saving={!!p} />
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
