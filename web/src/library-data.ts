export interface Profile {
  favorite: boolean;
  note: string;
}
export interface Session {
  id: string;
  game: string;
  started_ms: number;
  elapsed_ms: number;
}

export async function api<T>(path: string, body?: unknown): Promise<T> {
  const r = await fetch(
    new URL(`api/${path}`, location.href),
    body === undefined
      ? {}
      : {
          method: "PUT",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(body),
        },
  ).catch((e: unknown) => {
    throw navigator.onLine ? e : new Error("オフラインのため、プレイ記録は読み書きできません");
  });
  if (!r.ok) throw new Error(`保存サーバー: HTTP ${r.status}`);
  return (r.status === 204 ? undefined : await r.json()) as T;
}

/** Count foreground play, not loading or background tabs. Cumulative heartbeats are retry-safe. */
export function trackPlay(game: string): () => void {
  const session: Session = { id: crypto.randomUUID(), game, started_ms: Date.now(), elapsed_ms: 0 };
  let previous = performance.now();
  let visible = document.visibilityState === "visible";
  let stopped = false;
  const update = () => {
    const now = performance.now();
    if (visible) session.elapsed_ms += Math.max(0, Math.min(now - previous, 60_000));
    previous = now;
    visible = document.visibilityState === "visible";
  };
  const send = () => {
    update();
    void fetch(new URL("api/sessions", location.href), {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ ...session, elapsed_ms: Math.floor(session.elapsed_ms) }),
      keepalive: true,
    })
      .then((r) => {
        if (!r.ok) console.warn(`playtime: HTTP ${r.status}`);
      })
      .catch(console.warn);
  };
  const timer = setInterval(send, 15_000);
  const stop = () => {
    if (stopped) return;
    stopped = true;
    clearInterval(timer);
    send();
    removeEventListener("visibilitychange", send);
    removeEventListener("pagehide", stop);
  };
  addEventListener("visibilitychange", send);
  addEventListener("pagehide", stop);
  return stop;
}

/** Forgets all play time (DELETE /api/sessions): the statistics start again from nothing. */
export async function resetSessions(): Promise<void> {
  const r = await fetch(new URL("api/sessions", location.href), { method: "DELETE" });
  if (!r.ok) throw new Error(`保存サーバー: HTTP ${r.status}`);
}
