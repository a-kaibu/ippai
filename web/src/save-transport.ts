import { same } from "./file-records";
/** Bytes per second an upload is given at least before it is given up (a phone's poor link). */
const SLOW_LINK = 64 * 1024;

/** What a store that met another device's newer save rejects with (every later store of the
 * page too: the player must reload to take the other device's save). */
export class SaveConflict extends Error {
  constructor() {
    super("別の端末でセーブが更新されました。再読み込みが必要です");
    this.name = "SaveConflict";
  }
}

/** Serialize writes per game and keep the last acknowledged revision. Every request has a time
 * limit (`waitMs`, an upload also a second per SLOW_LINK bytes): one that stalls without failing
 * (a phone moving between networks) used to hold every later save of the game behind it. */
export class SaveTransport {
  private revisions = new Map<string, string>();
  private queues = new Map<string, Promise<void>>();
  /** what a game started from when the server could not be reached (this browser's copy) */
  private bases = new Map<string, Uint8Array>();
  // (fetch called as this.request would be an illegal invocation: bound to the window)
  constructor(
    private request: typeof fetch = (input, init) => fetch(input, init),
    private waitMs = 30_000,
  ) {}
  private limit(bytes = 0): AbortSignal {
    return AbortSignal.timeout(this.waitMs + Math.ceil((bytes / SLOW_LINK) * 1000));
  }
  async load(url: string): Promise<Uint8Array | null> {
    const r = await this.request(url, { cache: "no-store", signal: this.limit() });
    if (r.status === 404) {
      this.revisions.set(url, '"0"');
      return null;
    }
    if (!r.ok) throw new Error(`セーブの取得に失敗しました: HTTP ${r.status}`);
    const revision = r.headers.get("etag");
    if (!revision) throw new Error("セーブのバージョンがありません");
    const data = new Uint8Array(await r.arrayBuffer());
    this.revisions.set(url, revision);
    return data;
  }
  /**
   * The server was out of reach at the start, so the game started from `data` (this browser's
   * copy): the first store later learns the server's revision, and writes over it only if the
   * server still has these same bytes (or nothing), so another device's newer save is not lost.
   */
  startedOffline(url: string, data: Uint8Array | null, revision?: string | null): void {
    // (the revision this browser's copy was last in step with, kept beside it (saves.ts): the
    // server's If-Match then tells whether another device wrote since)
    if (revision) this.revisions.set(url, revision);
    else this.bases.set(url, data ?? new Uint8Array());
  }
  /** The server's revision this transport last saw for `url` (undefined: none yet). */
  revision(url: string): string | undefined {
    return this.revisions.get(url);
  }
  private async learnRevision(url: string): Promise<void> {
    const base = this.bases.get(url);
    if (!base) throw new Error("セーブの読み込みが未完了です");
    const r = await this.request(url, { cache: "no-store", signal: this.limit() });
    if (r.status === 404) {
      this.revisions.set(url, '"0"');
    } else {
      if (!r.ok) throw new Error(`セーブの取得に失敗しました: HTTP ${r.status}`);
      const revision = r.headers.get("etag");
      const bytes = new Uint8Array(await r.arrayBuffer());
      if (!revision || !same(bytes, base)) throw new SaveConflict();
      this.revisions.set(url, revision);
    }
    this.bases.delete(url);
  }
  store(url: string, data: Uint8Array<ArrayBuffer>): Promise<void> {
    // (the caller's array as it is: callers hand a fresh export and do not change it)
    const snapshot = data;
    const previous = this.queues.get(url) ?? Promise.resolve();
    const pending = previous
      .catch(() => {})
      .then(async () => {
        if (!this.revisions.has(url)) await this.learnRevision(url);
        const r = await this.request(url, {
          method: "PUT",
          headers: { "if-match": this.revisions.get(url)! },
          body: snapshot,
          keepalive: snapshot.length < 60000,
          signal: this.limit(snapshot.length),
        });
        if (r.status === 412) {
          // The write may have committed while its acknowledgement was lost. Accept only
          // byte-identical content; a genuinely different save must remain a conflict.
          const current = await this.request(url, { cache: "no-store", signal: this.limit(snapshot.length) });
          if (current.ok && current.headers.get("etag")) {
            const bytes = new Uint8Array(await current.arrayBuffer());
            if (same(bytes, snapshot)) {
              this.revisions.set(url, current.headers.get("etag")!);
              return;
            }
          }
          throw new SaveConflict();
        }
        if (!r.ok) throw new Error(`セーブの保存に失敗しました: HTTP ${r.status}`);
        const revision = r.headers.get("etag");
        if (!revision) throw new Error("保存結果のバージョンがありません");
        this.revisions.set(url, revision);
      });
    this.queues.set(url, pending);
    return pending;
  }
}
