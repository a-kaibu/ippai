/** Serialize writes per game and keep the last acknowledged revision. */
export class SaveTransport {
  private revisions = new Map<string, string>();
  private queues = new Map<string, Promise<void>>();
  // (fetch called as this.request would be an illegal invocation: bound to the window)
  constructor(private request: typeof fetch = (input, init) => fetch(input, init)) {}
  async load(url: string): Promise<Uint8Array | null> {
    const r = await this.request(url, { cache: "no-store" });
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
  store(url: string, data: Uint8Array<ArrayBuffer>): Promise<void> {
    const snapshot = data.slice();
    const previous = this.queues.get(url) ?? Promise.resolve();
    const pending = previous
      .catch(() => {})
      .then(async () => {
        if (!this.revisions.has(url)) throw new Error("セーブの読み込みが未完了です");
        const r = await this.request(url, {
          method: "PUT",
          headers: { "if-match": this.revisions.get(url)! },
          body: snapshot,
          keepalive: snapshot.length < 60000,
        });
        if (r.status === 412) {
          // The write may have committed while its acknowledgement was lost. Accept only
          // byte-identical content; a genuinely different save must remain a conflict.
          const current = await this.request(url, { cache: "no-store" });
          if (current.ok && current.headers.get("etag")) {
            const bytes = new Uint8Array(await current.arrayBuffer());
            if (bytes.length === snapshot.length && bytes.every((b, i) => b === snapshot[i])) {
              this.revisions.set(url, current.headers.get("etag")!);
              return;
            }
          }
          throw new Error("別の端末でセーブが更新されました。再読み込みが必要です");
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
