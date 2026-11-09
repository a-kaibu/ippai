import { describe, expect, it } from "vite-plus/test";
import { applyGame, nextRunUrl, type GameEntry } from "./launcher";

// anemoi from the list: Start.exe opens StartData\StartMenu.exe (ShellExecuteExW, /from="autorun"),
// which opens GameData\SiglusEngine.exe (/fullscreen)
const anemoi: GameEntry = {
  id: "anemoi",
  title: "anemoi",
  run: "Z:\\vnvm\\game\\Start.exe",
  args: [],
  mounts: ["/vnvm/game=anemoi"],
  env: ["SAKE_X=1"],
};

// a small seeded generator (the same cases every run)
function rng(seed: number) {
  return () => (seed = (seed * 1103515245 + 12345) >>> 0) / 2 ** 32;
}

describe("nextRunUrl", () => {
  it("keeps the mounts of a game picked from the list, whose address bar has only ?game=", () => {
    const params = new URLSearchParams("game=anemoi");
    applyGame(params, anemoi);
    params.set("saves", "Z__vnvm_game_Start.exe-c1b96914");
    const href = "https://w2.example/play.html?game=anemoi";
    const url = nextRunUrl(href, params, "Z:\\vnvm\\game\\StartData\\StartMenu.exe", '/from="autorun"');
    expect(url.origin + url.pathname).toBe("https://w2.example/play.html");
    expect(url.searchParams.get("game")).toBe("anemoi");
    expect(url.searchParams.getAll("mount")).toEqual(["/vnvm/game=anemoi"]);
    expect(url.searchParams.getAll("env")).toEqual(["SAKE_X=1"]);
    expect(url.searchParams.get("saves")).toBe("Z__vnvm_game_Start.exe-c1b96914");
    expect(url.searchParams.get("run")).toBe("Z:\\vnvm\\game\\StartData\\StartMenu.exe");
    expect(url.searchParams.get("args")).toBe('/from="autorun"');

    // the next exec, from that page (its params are its address)
    const url2 = nextRunUrl(
      url.href,
      new URLSearchParams(url.search),
      "Z:\\vnvm\\game\\StartData\\GameData\\SiglusEngine.exe",
      "/fullscreen",
    );
    expect(url2.searchParams.getAll("mount")).toEqual(["/vnvm/game=anemoi"]);
    expect(url2.searchParams.get("saves")).toBe("Z__vnvm_game_Start.exe-c1b96914");
    expect(url2.searchParams.get("args")).toBe("/fullscreen");
  });

  it("drops the arguments when there are none and a memory state of the program before", () => {
    const params = new URLSearchParams("run=a.exe&args=x,y&state=2&debug&mount=/vnvm/game=t");
    const url = nextRunUrl("http://h/play.html?run=a.exe", params, "b.exe", "  ");
    expect(url.searchParams.has("args")).toBe(false);
    expect(url.searchParams.has("state")).toBe(false);
    expect(url.searchParams.has("debug")).toBe(true);
    expect(url.searchParams.getAll("mount")).toEqual(["/vnvm/game=t"]);
    // (the page's own params are not changed)
    expect(params.get("run")).toBe("a.exe");
  });

  it("keeps every other parameter as it was, for any game (seeded property)", () => {
    const r = rng(7);
    const pick = (s: string) => s[Math.floor(r() * s.length)]!;
    const word = () =>
      Array.from({ length: 1 + Math.floor(r() * 8) }, () => pick('aZ09_-=&?%+ /\\:,"あ')).join("");
    for (let i = 0; i < 500; i++) {
      const g: GameEntry = {
        id: word(),
        title: word(),
        run: `Z:\\vnvm\\game\\${word()}.exe`,
        args: Array.from({ length: Math.floor(r() * 3) }, word).filter((a) => !/[\s,]/.test(a)),
        mounts: Array.from({ length: Math.floor(r() * 3) }, () => `/vnvm/${word()}=${word()}`),
        env: Array.from({ length: Math.floor(r() * 3) }, () => `${word()}=${word()}`),
      };
      const params = new URLSearchParams(r() < 0.5 ? "debug" : "");
      applyGame(params, g);
      params.set("saves", word());
      const before = new URLSearchParams(params);
      const args = Array.from({ length: Math.floor(r() * 4) }, word).join(" ");
      const run = `Z:\\vnvm\\game\\${word()}.exe`;
      const url = nextRunUrl(`http://h/play.html?game=${encodeURIComponent(g.id)}`, params, run, args);
      const got = url.searchParams;
      expect(params.toString()).toBe(before.toString());
      expect(got.get("run")).toBe(run);
      const want = args.split(/\s+/).filter(Boolean);
      expect(got.get("args")?.split(",") ?? []).toEqual(want.length ? want.join(",").split(",") : []);
      for (const k of new Set([...before.keys()].filter((k) => k !== "run" && k !== "args"))) {
        expect(got.getAll(k)).toEqual(before.getAll(k));
      }
    }
  });
});
