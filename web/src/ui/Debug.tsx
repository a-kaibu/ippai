// Debug mode over the game (the list's switch, ?debugmode=1 or Ctrl+Shift+D): network (bytes per
// second, requests, how long the game's threads waited for file data), memory and fps, from
// meter.ts. The same numbers go to the run log's perf lines every 10 s.

import { X } from "lucide-react";
import { cn } from "@/lib/utils";
import { mb, meter, useMeter, type MeterView } from "../meter";
import { setDebug } from "./store";

function Spark({ values }: { values: number[] }) {
  const w = 120;
  const h = 22;
  const max = Math.max(...values, 64 * 1024);
  const step = w / Math.max(values.length - 1, 1);
  const pts = values.map((v, i) => `${(i * step).toFixed(1)},${(h - (v / max) * h).toFixed(1)}`).join(" ");
  return (
    <svg width={w} height={h} className="mt-0.5 block" aria-hidden>
      <polyline points={pts} fill="none" stroke="currentColor" strokeWidth="1.25" className="text-sky-300" />
    </svg>
  );
}

const ms = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)} s` : `${n.toFixed(0)} ms`);

function Row({ k, children, bad }: { k: string; children: React.ReactNode; bad?: boolean }) {
  return (
    <div className="flex gap-2">
      <span className="w-14 shrink-0 text-white/60">{k}</span>
      <span className={cn("tabular-nums", bad && "font-semibold text-red-300")}>{children}</span>
    </div>
  );
}

function Lines({ v }: { v: MeterView }) {
  return (
    <>
      <Row k="帯域">
        {mb(v.wirePerSec)} MB/s ・ 累計 {mb(v.wireTotal)} MB
      </Row>
      <Spark values={meter.series()} />
      <Row k="要求" bad={v.failed > 0}>
        {v.requests} 件/{v.secs.toFixed(0)}s（キャッシュ {v.cached}
        {v.failed ? `・失敗 ${v.failed}` : ""}）・平均 {ms(v.reqAvgMs)}・最長 {ms(v.reqMaxMs)}・実行中{" "}
        {v.inflight}
      </Row>
      <Row k="読込待ち" bad={v.stallPerSec > 200}>
        {v.stallPerSec.toFixed(0)} ms/s（最長 {ms(v.stallMaxMs)}）
      </Row>
      <Row k="メモリ">
        {v.heapUsed >= 0 ? `wasm ${mb(v.heapUsed)} MB（最大 ${mb(v.heapTop)}）・` : ""}
        {v.jsHeap >= 0 ? `JS ${mb(v.jsHeap)} MB・` : ""}キャッシュ {mb(v.cacheBytes)} MB
      </Row>
      <Row k="fps">
        ゲーム {v.gameFps.toFixed(0)}・ページ {v.pageFps.toFixed(0)}・CPU {v.cpu.toFixed(2)} コア
      </Row>
    </>
  );
}

export function DebugOverlay() {
  const v = useMeter();
  return (
    <div
      data-debug-overlay
      className="fixed top-1 right-1 z-50 max-w-[calc(100%-0.5rem)] rounded-md bg-black/75 px-2 py-1.5 font-mono text-[11px] leading-snug text-white shadow-lg sm:text-xs"
    >
      <div className="mb-0.5 flex items-center gap-2">
        <span className="font-semibold text-amber-300">デバッグ</span>
        <button
          type="button"
          onClick={() => setDebug(false)}
          aria-label="デバッグ表示を閉じる"
          className="ml-auto grid size-5 place-items-center rounded text-white/70 hover:bg-white/15 hover:text-white"
        >
          <X className="size-3.5" />
        </button>
      </div>
      {v ? <Lines v={v} /> : <div className="text-white/60">ゲームの起動を待っています</div>}
    </div>
  );
}
