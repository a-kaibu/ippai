import { describe as group, expect, it } from "vite-plus/test";
import tsv from "../../testdata/meta-urls.tsv?raw";
import { describe, looksLikeGamePage, untilSettled, type MetaStatus } from "./meta-source";

// the same table as the server's parser (crates/ippai/src/meta_url.rs, test `pages`): the page
// lets through exactly what the server takes
const rows = tsv
  .split("\n")
  .filter((l) => l && !l.startsWith("#"))
  .map((l) => {
    const [url, want] = l.split("\t");
    return [url.replaceAll("\\n", "\n").replaceAll("\\t", "\t"), want !== "-"] as const;
  });

group("looksLikeGamePage", () => {
  it("agrees with the server on every row of testdata/meta-urls.tsv", () => {
    expect(rows.length).toBeGreaterThan(30);
    for (const [url, want] of rows) expect([url, looksLikeGamePage(url)]).toEqual([url, want]);
  });

  it("takes any game id on either site, with parameters and a fragment around it", () => {
    let seed = 7;
    const next = () => (seed = (seed * 1103515245 + 12345) >>> 0);
    for (let i = 0; i < 500; i++) {
      const n = (next() % 999_999_999) + 1;
      const extra = next().toString(36);
      expect(
        looksLikeGamePage(
          `https://erogamescape.dyndns.org/~ap2/ero/toukei_kaiseki/game.php?a=${extra}&game=${n}#${extra}`,
        ),
      ).toBe(true);
      expect(looksLikeGamePage(`https://vndb.org/v${n}/${extra}`)).toBe(true);
      expect(looksLikeGamePage(`https://example.com/v${n}`)).toBe(false);
    }
  });
});

group("describe", () => {
  it("says what the last fetch did", () => {
    expect(describe(null)).toBe("");
    const s: MetaStatus = { url: "egs:1", state: "pending", error: "", updated_ms: 0 };
    expect(describe(s)).toBe("取得しています…");
    expect(describe({ ...s, state: "error", error: "vndb: timed out" })).toBe(
      "取得できませんでした：vndb: timed out",
    );
    expect(describe({ ...s, state: "ok", updated_ms: Date.UTC(2026, 10, 9) })).toContain("に更新しました");
  });
});

group("untilSettled", () => {
  it("polls while pending and stops at the end, reporting each state", async () => {
    const states: (MetaStatus | null)[] = ["pending", "pending", "ok"].map((state) => ({
      url: "egs:1",
      state: state as MetaStatus["state"],
      error: "",
      updated_ms: 1,
    }));
    const seen: string[] = [];
    let reads = 0;
    const end = await untilSettled(
      "g",
      (s) => seen.push(s?.state ?? "none"),
      new AbortController().signal,
      1,
      async () => states[reads++],
    );
    expect(end?.state).toBe("ok");
    expect(seen).toEqual(["pending", "pending", "ok"]);
  });

  it("stops when aborted (the dialog closed)", async () => {
    const stop = new AbortController();
    stop.abort();
    let reads = 0;
    const end = await untilSettled(
      "g",
      () => undefined,
      stop.signal,
      1,
      async () => {
        reads++;
        return null;
      },
    );
    expect([end, reads]).toEqual([null, 0]);
  });
});
