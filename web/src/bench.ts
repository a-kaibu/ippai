// bench.exe (hangover/tests/bench.c) in wine.html, with native Wine's numbers next to ours.
//
//   /bench.html?scale=3    repeat count (default 3); times are shown per repeat
//   &passes=2              runs (default 2): the page reloads itself after each, so later runs
//                          start from the regions earlier ones saved
//   (other wine.html parameters pass through)
//
// It starts by itself, and each run's result goes to vnvm-server's `PUT /bench-log`, which
// prints it and appends it to bench-results.txt.

// native Wine on the x86-64 dev machine, bench.exe 1 (docs/hangover.md)
const NATIVE: Record<string, number> = {
  crc32: 53,
  lz: 30,
  blend: 6,
  memcpy: 155,
  sort: 123,
  float: 7,
  calls: 39,
  total: 413,
};

const params = new URLSearchParams(location.search);
const scale = Math.max(1, Number(params.get("scale") ?? 3) || 1);
const passes = Math.max(1, Number(params.get("passes") ?? 2) || 1);
const pass = Math.max(1, Number(params.get("pass") ?? 1) || 1);
if (!params.has("run")) {
  params.set("run", "C:\\bench.exe");
  params.set("args", String(scale));
  params.set("autostart", "");
  history.replaceState(null, "", `?${params}`);
}

const table = document.querySelector<HTMLTableElement>("#result")!;
const env = document.querySelector<HTMLDivElement>("#env")!;
const copy = document.querySelector<HTMLButtonElement>("#copy")!;
table.innerHTML = "<tr><th></th><th>ms</th><th>native</th><th>倍</th></tr>";
env.textContent = `${navigator.userAgent} / ${navigator.hardwareConcurrency} cores / scale ${scale} / run ${pass} of ${passes}`;
const lines: string[] = [];
const t0 = performance.now();

addEventListener("wine-print", (e) => {
  const line = (e as CustomEvent<string>).detail;
  const m = /^(\w+)\s+([\d.]+) ms\s+([0-9a-f]+)/.exec(line);
  if (!m) return;
  const [, name, ms, crc] = m as unknown as [string, string, string, string];
  const per = Number(ms) / scale;
  const native = NATIVE[name];
  const row = table.insertRow();
  for (const text of [name, per.toFixed(0), native?.toString() ?? "", native ? (per / native).toFixed(2) : ""])
    row.insertCell().textContent = text;
  lines.push(`${name} ${per.toFixed(1)} ${crc}`);
  if (name === "total") {
    lines.push(`(start to end ${((performance.now() - t0) / 1000).toFixed(1)} s)`, env.textContent!);
    copy.hidden = false;
    void fetch("bench-log", { method: "PUT", body: lines.join("\n") })
      .catch(() => undefined)
      .then(() => {
        if (pass >= passes) return;
        params.set("pass", String(pass + 1));
        location.search = `?${params}`;
      });
  }
});
copy.addEventListener("click", () => void navigator.clipboard.writeText(lines.join("\n")));

await import("./wine");

export {};
