import { describe, expect, it } from "vite-plus/test";
import { applyGame, listUrl, needsListBelow, nextRunUrl, type GameEntry } from "./launcher";

// anemoi from the list: Start.exe opens StartData\StartMenu.exe (ShellExecuteExW, /from="autorun"),
// which opens GameData\SiglusEngine.exe (/fullscreen)
const anemoi: GameEntry = {
  id: "anemoi",
  title: "anemoi",
  run: "Z:\\ippai\\game\\Start.exe",
  args: [],
  mounts: ["/ippai/game=anemoi"],
  env: ["SAKE_X=1"],
};

// a small seeded generator (the same cases every run)
function rng(seed: number) {
  return () => (seed = (seed * 1103515245 + 12345) >>> 0) / 2 ** 32;
}

describe("applyGame", () => {
  it("keeps the address's own env after the game's (a trace asked for in the URL)", () => {
    const params = new URLSearchParams("game=anemoi&env=MASU_D3D_TRACE%3D1&env=SAKE_X%3D2");
    applyGame(params, anemoi);
    expect(params.getAll("env")).toEqual(["SAKE_X=1", "MASU_D3D_TRACE=1", "SAKE_X=2"]);
    // (the page's environment: the last of a name wins)
    const env = Object.fromEntries(
      params.getAll("env").map((e) => [e.slice(0, e.indexOf("=")), e.slice(e.indexOf("=") + 1)]),
    );
    expect(env).toEqual({ SAKE_X: "2", MASU_D3D_TRACE: "1" });
    // applied again (a page that already has them): the same, not doubled
    applyGame(params, anemoi);
    expect(params.getAll("env")).toEqual(["SAKE_X=1", "MASU_D3D_TRACE=1", "SAKE_X=2"]);
  });
});

describe("applyGame launcher", () => {
  it("names the launcher a Siglus game was registered with (its saves are read), and drops it for another game", () => {
    const params = new URLSearchParams("game=anemoi");
    applyGame(params, {
      ...anemoi,
      run: "Z:\\ippai\\game\\StartData\\GameData\\data\\SiglusEngine.exe",
      launcher: "Z:\\ippai\\game\\Start.exe",
    });
    expect(params.get("run")).toBe("Z:\\ippai\\game\\StartData\\GameData\\data\\SiglusEngine.exe");
    expect(params.get("launcher")).toBe("Z:\\ippai\\game\\Start.exe");
    applyGame(params, anemoi);
    expect(params.has("launcher")).toBe(false);
  });
});

describe("nextRunUrl", () => {
  it("keeps the mounts of a game picked from the list, whose address bar has only ?game=", () => {
    const params = new URLSearchParams("game=anemoi");
    applyGame(params, anemoi);
    params.set("saves", "Z__ippai_game_Start.exe-c1b96914");
    const href = "https://w2.example/play.html?game=anemoi";
    const url = nextRunUrl(href, params, "Z:\\ippai\\game\\StartData\\StartMenu.exe", '/from="autorun"');
    expect(url.origin + url.pathname).toBe("https://w2.example/play.html");
    expect(url.searchParams.get("game")).toBe("anemoi");
    expect(url.searchParams.getAll("mount")).toEqual(["/ippai/game=anemoi"]);
    expect(url.searchParams.getAll("env")).toEqual(["SAKE_X=1"]);
    expect(url.searchParams.get("saves")).toBe("Z__ippai_game_Start.exe-c1b96914");
    expect(url.searchParams.get("run")).toBe("Z:\\ippai\\game\\StartData\\StartMenu.exe");
    expect(url.searchParams.get("args")).toBe('/from="autorun"');
    // (the player pressed 起動 for the launcher: the next program starts by itself)
    expect(url.searchParams.has("autostart")).toBe(true);

    // the next exec, from that page (its params are its address)
    const url2 = nextRunUrl(
      url.href,
      new URLSearchParams(url.search),
      "Z:\\ippai\\game\\StartData\\GameData\\SiglusEngine.exe",
      "/fullscreen",
    );
    expect(url2.searchParams.getAll("mount")).toEqual(["/ippai/game=anemoi"]);
    expect(url2.searchParams.get("saves")).toBe("Z__ippai_game_Start.exe-c1b96914");
    expect(url2.searchParams.get("args")).toBe("/fullscreen");
  });

  it("drops the arguments when there are none and a memory state of the program before", () => {
    const params = new URLSearchParams("run=a.exe&args=x,y&state=2&debug&mount=/ippai/game=t");
    const url = nextRunUrl("http://h/play.html?run=a.exe", params, "b.exe", "  ");
    expect(url.searchParams.has("args")).toBe(false);
    expect(url.searchParams.has("state")).toBe(false);
    expect(url.searchParams.has("debug")).toBe(true);
    expect(url.searchParams.getAll("mount")).toEqual(["/ippai/game=t"]);
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
        run: `Z:\\ippai\\game\\${word()}.exe`,
        args: Array.from({ length: Math.floor(r() * 3) }, word).filter((a) => !/[\s,]/.test(a)),
        mounts: Array.from({ length: Math.floor(r() * 3) }, () => `/masu/${word()}=${word()}`),
        env: Array.from({ length: Math.floor(r() * 3) }, () => `${word()}=${word()}`),
      };
      const params = new URLSearchParams(r() < 0.5 ? "debug" : "");
      applyGame(params, g);
      params.set("saves", word());
      const before = new URLSearchParams(params);
      const args = Array.from({ length: Math.floor(r() * 4) }, word).join(" ");
      const run = `Z:\\ippai\\game\\${word()}.exe`;
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

describe("listUrl", () => {
  it("is the list's address under a game's page", () => {
    const url = listUrl("https://w2.example/play.html?game=anemoi");
    expect(url.href).toBe("https://w2.example/play.html");
  });
  it("drops the game, its autostart and a state slot, and keeps the page's switches", () => {
    const url = listUrl("https://w2.example/?jit=0&game=anemoi&autostart&state=3&memory=grow");
    expect([...url.searchParams]).toEqual([
      ["jit", "0"],
      ["memory", "grow"],
    ]);
    expect(url.pathname).toBe("/");
  });
  it("never names a game, whatever the query (random queries)", () => {
    const next = rng(7);
    const keys = ["game", "autostart", "state", "jit", "memory", "mount", "x"];
    for (let i = 0; i < 500; i++) {
      const q = new URLSearchParams();
      const n = Math.floor(next() * 8);
      for (let j = 0; j < n; j++)
        q.append(keys[Math.floor(next() * keys.length)], String(Math.floor(next() * 9)));
      const url = listUrl(`https://w2.example/play.html?${q}`);
      expect(url.searchParams.has("game")).toBe(false);
      expect(url.searchParams.has("autostart")).toBe(false);
      expect(url.searchParams.has("state")).toBe(false);
      const kept = [...q].filter(([k]) => !["game", "autostart", "state"].includes(k));
      expect([...url.searchParams]).toEqual(kept);
      // (idempotent: the list of the list is the list)
      expect(listUrl(url.href).href).toBe(url.href);
    }
  });
});

describe("needsListBelow", () => {
  it("is true for a page opened at its own address (no state, or another game's)", () => {
    expect(needsListBelow(null, "anemoi")).toBe(true);
    expect(needsListBelow(undefined, "anemoi")).toBe(true);
    expect(needsListBelow({ game: "reunion" }, "anemoi")).toBe(true);
    expect(needsListBelow("anemoi", "anemoi")).toBe(true);
  });
  it("is false once the list is under it (picked from the list, or after a reload)", () => {
    expect(needsListBelow({ game: "anemoi" }, "anemoi")).toBe(false);
  });
});
