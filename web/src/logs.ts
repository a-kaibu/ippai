// logs.html: the runs' logs (web/src/crashlog.ts) on the server (vnvm-server GET /log-list,
// files under /logs/) and in this browser's OPFS, to read, download or send.

import { listLocal, readLocal, sendLeftovers, upload } from "./crashlog";

const view = document.querySelector<HTMLPreElement>("#view")!;

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  cls?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function kind(name: string): string {
  return /-crash\d*(\.txt)?$/.test(name) ? "crash" : /-unclean(\.txt)?$/.test(name) ? "unclean" : "";
}

function row(name: string, size: number, time: number): HTMLLIElement {
  const li = el("li", kind(name));
  li.append(
    el("span", "name", name),
    el("span", "meta", `${new Date(time).toLocaleString()} · ${(size / 1024).toFixed(0)} KB`),
  );
  return li;
}

function show(text: string): void {
  view.textContent = text;
  view.hidden = false;
  view.scrollIntoView();
}

function download(name: string, text: string): void {
  const a = el("a");
  a.href = URL.createObjectURL(new Blob([text], { type: "text/plain;charset=utf-8" }));
  a.download = name.endsWith(".txt") ? name : `${name}.txt`;
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 10_000);
}

async function server(): Promise<void> {
  const ul = document.querySelector<HTMLUListElement>("#server")!;
  try {
    const r = await fetch("log-list");
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    const list = (await r.json()) as { name: string; size: number; time: number }[];
    ul.replaceChildren(
      ...list.map((f) => {
        const li = row(f.name, f.size, f.time);
        const a = el("a", undefined, "開く");
        a.href = `logs/${encodeURIComponent(f.name)}`;
        a.target = "_blank";
        li.append(a);
        return li;
      }),
    );
    if (!list.length) ul.replaceChildren(el("li", "empty", "まだありません"));
  } catch (e) {
    ul.replaceChildren(el("li", "empty", `一覧を取れませんでした: ${e}`));
  }
}

async function local(): Promise<void> {
  const ul = document.querySelector<HTMLUListElement>("#local")!;
  const list = await listLocal();
  ul.replaceChildren(
    ...list.map((f) => {
      const li = row(f.name, f.size, f.time);
      const open = el("button", undefined, "表示");
      open.onclick = async () => show(await readLocal(f.name));
      const save = el("button", undefined, "ダウンロード");
      save.onclick = async () => download(f.name, await readLocal(f.name));
      const send = el("button", undefined, "サーバーへ送る");
      send.onclick = async () => {
        send.disabled = true;
        const ok = await upload(f.name.replace(/\.txt$/, ""), await readLocal(f.name));
        send.textContent = ok ? "送信済み" : "送れませんでした";
        if (ok) void server();
      };
      li.append(open, save, send);
      return li;
    }),
  );
  if (!list.length) ul.replaceChildren(el("li", "empty", "まだありません"));
}

await sendLeftovers().catch(() => {});
await Promise.all([server(), local()]);
