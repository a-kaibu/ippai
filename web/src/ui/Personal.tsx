// The player's own records on the server (ippai serve /api): a game's favorite flag and note,
// its backup (.ippai: saves and note), and the play time statistics.

import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { Clock, Download, Gamepad2, Loader2, RotateCcw, Save, Upload } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { api, resetSessions, type Profile, type Session } from "../library-data";
import { coverUrl, type GameEntry } from "../launcher";
import { duration, playTotals, when } from "../library-view";

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** A game's record: loaded once, the favorite flag saved at once, the note on 保存. */
export function useProfile(game: string, onFavorite?: (on: boolean) => void) {
  const [profile, setProfile] = useState<Profile | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const saved = useRef<Profile | null>(null);
  const load = () =>
    api<Profile>(`library/${encodeURIComponent(game)}`).then((p) => {
      saved.current = p;
      setProfile(p);
      setError("");
    });
  useEffect(() => {
    let active = true;
    setProfile(null);
    api<Profile>(`library/${encodeURIComponent(game)}`)
      .then((p) => {
        if (!active) return;
        saved.current = p;
        setProfile(p);
      })
      .catch((e: unknown) => active && setError(message(e)));
    return () => {
      active = false;
    };
  }, [game]);
  const put = async (next: Profile) => {
    setBusy(true);
    try {
      await api(`library/${encodeURIComponent(game)}`, next);
      saved.current = next;
      return true;
    } catch (e) {
      toast.error(`保存できませんでした: ${message(e)}`);
      return false;
    } finally {
      setBusy(false);
    }
  };
  return {
    profile,
    error,
    busy,
    dirty: !!profile && !!saved.current && profile.note !== saved.current.note,
    setNote: (note: string) => profile && setProfile({ ...profile, note }),
    toggleFavorite: async () => {
      if (!profile || !saved.current) return;
      // (only the flag changes: an unsaved note stays unsaved)
      const next = { ...saved.current, favorite: !profile.favorite };
      setProfile({ ...profile, favorite: next.favorite });
      if (await put(next)) onFavorite?.(next.favorite);
      else setProfile({ ...profile });
    },
    saveNote: async () => {
      if (!profile || !saved.current) return;
      if (await put({ ...saved.current, note: profile.note })) toast.success("メモを保存しました");
    },
    reload: load,
  };
}

export type ProfileState = ReturnType<typeof useProfile>;

export function Note({ state }: { state: ProfileState }) {
  if (state.error) return <p className="text-sm text-muted-foreground">{state.error}</p>;
  if (!state.profile) return <Skeleton className="h-24 w-full" />;
  return (
    <div className="grid gap-2">
      <textarea
        aria-label="メモ"
        placeholder="攻略の進み具合や感想など"
        className="min-h-24 w-full resize-y rounded-md border border-input bg-input/30 px-3 py-2 text-sm shadow-xs outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
        maxLength={25000}
        value={state.profile.note}
        onChange={(e) => state.setNote(e.target.value)}
      />
      <div className="flex justify-end">
        <Button
          size="sm"
          variant="secondary"
          disabled={!state.dirty || state.busy}
          onClick={() => void state.saveNote()}
        >
          {state.busy && state.dirty ? <Loader2 className="animate-spin" /> : <Save />} メモを保存
        </Button>
      </div>
    </div>
  );
}

/** The game's saves and note as one .ippai file, and back. */
export function Backup({ game, state }: { game: string; state: ProfileState }) {
  const file = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const url = `api/packages/${encodeURIComponent(game)}`;
  const restore = (f: File) => {
    if (!confirm(`${f.name} から復元します。今のセーブデータとメモは置き換わります。`)) return;
    setBusy(true);
    void fetch(new URL(url, location.href), { method: "PUT", body: f })
      .then(async (r) => {
        if (!r.ok) throw new Error((await r.text()) || `HTTP ${r.status}`);
        await state.reload();
        toast.success("復元しました");
      })
      .catch((e: unknown) => toast.error(`復元できませんでした: ${message(e)}`))
      .finally(() => {
        setBusy(false);
        if (file.current) file.current.value = "";
      });
  };
  return (
    <div className="flex flex-wrap gap-2">
      <Button asChild size="sm" variant="outline">
        <a href={url} download>
          <Download /> 書き出す（.ippai）
        </a>
      </Button>
      <Button size="sm" variant="outline" disabled={busy} onClick={() => file.current?.click()}>
        {busy ? <Loader2 className="animate-spin" /> : <Upload />} ファイルから復元
      </Button>
      <input
        ref={file}
        type="file"
        accept=".ippai"
        className="hidden"
        aria-label=".ippai から復元"
        onChange={(e) => {
          const f = e.target.files?.[0];
          if (f) restore(f);
        }}
      />
    </div>
  );
}

function Tile({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-lg border bg-card px-3 py-2.5">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="mt-0.5 text-base font-semibold tabular-nums sm:text-lg">{value}</div>
    </div>
  );
}

/** Play time per game; 統計をリセット (after a second click to confirm) forgets all of it. */
export function Statistics({ games }: { games: GameEntry[] }) {
  const [sessions, setSessions] = useState<Session[] | null>(null);
  const [error, setError] = useState("");
  const [confirm, setConfirm] = useState(false);
  const [resetting, setResetting] = useState(false);
  const reset = async () => {
    setResetting(true);
    try {
      await resetSessions();
      setSessions([]);
      setConfirm(false);
      toast.success("プレイ統計をリセットしました");
    } catch (e) {
      toast.error(`リセットできませんでした: ${message(e)}`);
    } finally {
      setResetting(false);
    }
  };
  useEffect(() => {
    void api<Session[]>("sessions")
      .then(setSessions)
      .catch((e: unknown) => setError(message(e)));
  }, []);
  if (error)
    return (
      <p role="alert" className="text-sm text-destructive">
        {error}
      </p>
    );
  if (!sessions)
    return (
      <div role="status" aria-label="統計を読み込み中" className="grid gap-2">
        <Skeleton className="h-16 w-full" />
        <Skeleton className="h-10 w-full" />
        <Skeleton className="h-10 w-full" />
      </div>
    );
  const totals = playTotals(sessions);
  const all = totals.reduce((n, t) => n + t.ms, 0);
  const top = totals[0]?.ms ?? 1;
  return (
    <section className="grid gap-4" aria-label="プレイ時間の統計">
      <div className="grid grid-cols-3 gap-2">
        <Tile label="合計時間" value={duration(all)} />
        <Tile label="ゲーム" value={`${totals.length} 本`} />
        <Tile label="起動回数" value={`${totals.reduce((n, t) => n + t.sessions, 0)} 回`} />
      </div>
      {!totals.length ? (
        <p className="rounded-lg border border-dashed p-6 text-center text-sm text-muted-foreground">
          プレイ記録はまだありません
        </p>
      ) : (
        <ol className="grid gap-1" aria-label="ゲーム別のプレイ時間">
          {totals.map((t) => {
            const g = games.find((x) => x.id === t.game);
            const cover = g && coverUrl(g);
            return (
              <li key={t.game} className="flex items-center gap-3 rounded-lg px-1 py-1.5">
                <div className="grid size-10 shrink-0 place-items-center overflow-hidden rounded-md bg-muted">
                  {cover ? (
                    <img src={cover} alt="" className="size-full object-cover" />
                  ) : (
                    <Gamepad2 className="size-4 text-muted-foreground" />
                  )}
                </div>
                <div className="min-w-0 flex-1">
                  <div className="flex items-baseline gap-2">
                    <span className="truncate text-sm font-medium">{g?.title ?? t.game}</span>
                    <span className="ml-auto shrink-0 text-sm tabular-nums">{duration(t.ms)}</span>
                  </div>
                  <div className="mt-1 h-1.5 overflow-hidden rounded-full bg-muted">
                    <div
                      className="h-full rounded-full bg-primary"
                      style={{ width: `${(t.ms / top) * 100}%` }}
                    />
                  </div>
                  <div className="mt-1 flex items-center gap-1 text-xs text-muted-foreground">
                    <Clock className="size-3" /> 最後に遊んだ日 {when(t.last)}・{t.sessions} 回
                  </div>
                </div>
              </li>
            );
          })}
        </ol>
      )}
      <p className="text-xs text-muted-foreground">
        ゲームを起動してから、タブを表示している時間を記録しています。
      </p>
      {totals.length > 0 &&
        (confirm ? (
          <div
            role="alertdialog"
            aria-label="統計のリセットの確認"
            className="grid gap-2 rounded-lg border border-destructive/40 p-3 text-sm"
          >
            <p>すべてのゲームのプレイ時間と起動回数を消します。元には戻せません。</p>
            <div className="flex flex-wrap justify-end gap-2">
              <Button size="sm" variant="outline" disabled={resetting} onClick={() => setConfirm(false)}>
                やめる
              </Button>
              <Button size="sm" variant="destructive" disabled={resetting} onClick={() => void reset()}>
                {resetting ? <Loader2 className="animate-spin" /> : <RotateCcw />} リセットする
              </Button>
            </div>
          </div>
        ) : (
          <Button size="sm" variant="outline" className="justify-self-start" onClick={() => setConfirm(true)}>
            <RotateCcw /> 統計をリセット
          </Button>
        ))}
    </section>
  );
}
