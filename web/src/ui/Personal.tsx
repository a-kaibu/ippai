import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { api, type Profile, type Session } from "../library-data";
import type { GameEntry } from "../launcher";

export function Personal({ game }: { game: string }) {
  const [profile, setProfile] = useState<Profile | null>(null);
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    let active = true;
    api<Profile>(`library/${encodeURIComponent(game)}`)
      .then((p) => {
        if (active) setProfile(p);
      })
      .catch((e: unknown) => {
        if (active) setMessage(String(e));
      });
    return () => {
      active = false;
    };
  }, [game]);
  const save = async () => {
    setBusy(true);
    try {
      await api(`library/${encodeURIComponent(game)}`, profile);
      setMessage("保存しました");
    } catch (e) {
      setMessage(String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className="grid gap-3" aria-label="プレイ記録" aria-busy={busy}>
      {profile ? (
        <>
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={profile.favorite}
              onChange={(e) => setProfile({ ...profile, favorite: e.target.checked })}
            />
            お気に入り
          </label>
          <label className="grid gap-2">
            メモ
            <textarea
              className="min-h-24 rounded border bg-background p-2"
              maxLength={25000}
              value={profile.note}
              onChange={(e) => setProfile({ ...profile, note: e.target.value })}
            />
          </label>
          <Button variant="outline" disabled={busy} onClick={() => void save()}>
            記録を保存
          </Button>
        </>
      ) : (
        <p>記録を読み込み中</p>
      )}
      <a className="underline" href={`api/packages/${encodeURIComponent(game)}`}>
        .ippai を書き出す
      </a>
      <label className="grid gap-2">
        .ippai から復元（現在のセーブとメモを置き換えます）
        <input
          type="file"
          accept=".ippai"
          disabled={busy}
          onChange={(e) => {
            const file = e.target.files?.[0];
            if (!file) return;
            setBusy(true);
            void fetch(new URL(`api/packages/${encodeURIComponent(game)}`, location.href), {
              method: "PUT",
              body: file,
            })
              .then(async (r) => {
                if (!r.ok) throw new Error(await r.text());
                setProfile(await api<Profile>(`library/${encodeURIComponent(game)}`));
                setMessage("復元しました");
              })
              .catch((error: unknown) => setMessage(String(error)))
              .finally(() => setBusy(false));
          }}
        />
      </label>
      <p role="status" className="text-sm">
        {message}
      </p>
    </section>
  );
}

const duration = (ms: number) => `${Math.floor(ms / 3600000)}時間 ${Math.floor(ms / 60000) % 60}分`;

export function Statistics({ games }: { games: GameEntry[] }) {
  const [sessions, setSessions] = useState<Session[] | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    void api<Session[]>("sessions")
      .then(setSessions)
      .catch((e: unknown) => setError(String(e)));
  }, []);
  if (error) return <p role="alert">{error}</p>;
  if (!sessions) return <p role="status">統計を読み込み中</p>;
  const totals = new Map<string, number>();
  for (const s of sessions) totals.set(s.game, (totals.get(s.game) ?? 0) + s.elapsed_ms);
  const ordered = [...totals].sort((a, b) => b[1] - a[1]);
  return (
    <section className="grid gap-4" aria-label="プレイ時間の統計">
      <p>
        合計 {duration(sessions.reduce((n, s) => n + s.elapsed_ms, 0))} ／ {totals.size} 本
      </p>
      <p className="text-sm text-muted-foreground">ゲーム起動後、タブを表示している時間を記録します。</p>
      {!sessions.length ? (
        <p>プレイ記録はまだありません。</p>
      ) : (
        <table className="w-full text-left text-sm">
          <caption className="sr-only">ゲーム別の累計プレイ時間</caption>
          <thead>
            <tr>
              <th scope="col">ゲーム</th>
              <th scope="col">プレイ時間</th>
            </tr>
          </thead>
          <tbody>
            {ordered.map(([id, ms]) => (
              <tr key={id}>
                <th scope="row" className="py-2 font-normal">
                  {games.find((g) => g.id === id)?.title ?? id}
                </th>
                <td>{duration(ms)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}
