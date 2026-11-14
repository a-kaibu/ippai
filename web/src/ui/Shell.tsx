// Over a game's screen: the status line, the start button, the cover while loading, &d3dstats
// and the bench panel (state in ui/store.ts), plus toasts (sonner).

import { createRoot } from "react-dom/client";
import { ArrowLeft, Copy, Loader2, Play, WifiOff } from "lucide-react";
import type { ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Toaster } from "@/components/ui/sonner";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { cn } from "@/lib/utils";
import { States } from "./States";
import { useUi, type BenchView } from "./store";

function BenchPanel({ bench }: { bench: BenchView }) {
  const [head, ...rows] = bench.rows;
  return (
    <div className="fixed top-2 left-2 z-10 max-h-[calc(100%-4rem)] max-w-[calc(100%-1rem)] overflow-auto rounded-lg border bg-background/85 p-3 shadow-lg backdrop-blur">
      <div
        className={cn(
          "mb-2 flex items-center gap-2 rounded-md px-3 py-2 text-base font-semibold text-white",
          bench.done ? "bg-emerald-700" : "bg-amber-700",
        )}
      >
        {!bench.done && <Loader2 className="size-4 animate-spin" />}
        {bench.phase}
      </div>
      {head && (
        <Table className="tabular-nums">
          <TableHeader>
            <TableRow>
              {head.map((h, i) => (
                <TableHead key={i} className={i ? "text-right" : ""}>
                  {h}
                </TableHead>
              ))}
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((r, i) => (
              <TableRow key={i}>
                {r.map((c, j) => (
                  <TableCell key={j} className={cn("py-1", j ? "text-right" : "")}>
                    {c}
                  </TableCell>
                ))}
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {bench.choices && (
        <div className="mb-2 flex flex-wrap gap-2">
          {bench.choices.map((c) => (
            <Button key={c.label} size="lg" className="h-auto flex-col items-start py-2" onClick={c.onClick}>
              <span className="text-base font-semibold">{c.label}</span>
              {c.detail && <span className="text-xs font-normal opacity-80">{c.detail}</span>}
            </Button>
          ))}
        </div>
      )}
      <p className="mt-2 text-[11px] break-all text-muted-foreground">{bench.env}</p>
      {bench.copy && (
        <Button size="sm" variant="secondary" className="mt-2" onClick={bench.copy}>
          <Copy /> 結果をコピー
        </Button>
      )}
    </div>
  );
}

function Shell({ children }: { children?: ReactNode }) {
  const ui = useUi();
  return (
    <>
      {ui.splash && (
        <div
          className="pointer-events-none fixed inset-0 bg-black bg-contain bg-center bg-no-repeat opacity-35"
          style={{ backgroundImage: `url("${ui.splash}")` }}
        />
      )}
      {children}
      {ui.start.shown && (
        <div className="fixed inset-0 z-20 grid place-items-center">
          <Button
            size="lg"
            className="h-12 rounded-full px-8 text-base shadow-xl"
            disabled={ui.start.disabled}
            onClick={ui.start.onClick}
          >
            <Play className="fill-current" /> {ui.start.label}
          </Button>
        </div>
      )}
      {ui.status && (
        <div className="pointer-events-none fixed bottom-3 left-3 z-30 flex max-w-[calc(100%-1.5rem)] items-center gap-2 rounded-full border bg-background/80 px-3 py-1.5 text-sm shadow backdrop-blur">
          {!/^エラー|ません/.test(ui.status) && <Loader2 className="size-3.5 shrink-0 animate-spin" />}
          <span className="truncate">{ui.status}</span>
        </div>
      )}
      {ui.net && (
        <div
          role="status"
          className="pointer-events-none fixed top-3 left-1/2 z-30 flex -translate-x-1/2 items-center gap-2 rounded-full border bg-background/85 px-3 py-1.5 text-sm shadow backdrop-blur"
        >
          <WifiOff className="size-3.5 shrink-0" />
          <span>回線が不安定です。再接続を待っています</span>
        </div>
      )}
      {ui.stats.length > 0 && (
        <pre className="pointer-events-none fixed top-1 left-1 z-30 m-0 max-w-[calc(100%-1rem)] rounded bg-black/70 px-1.5 py-1 font-mono text-[11px] leading-tight whitespace-pre-wrap text-white">
          {ui.stats.join("\n")}
        </pre>
      )}
      {ui.bench && <BenchPanel bench={ui.bench} />}
      {(ui.states || ui.auto) && !ui.back && <States states={ui.states} auto={ui.auto} />}
      {ui.back && (
        <div className="fixed inset-0 z-30 grid place-items-center">
          <Button
            size="lg"
            variant="secondary"
            className="h-12 rounded-full px-6 shadow-xl"
            onClick={ui.back}
          >
            <ArrowLeft /> 一覧に戻る
          </Button>
        </div>
      )}
      <Toaster position="bottom-center" />
    </>
  );
}

let root: ReturnType<typeof createRoot> | null = null;

/** Renders the shell (with `children`, e.g. the game list) into #ui. */
export function renderShell(children?: ReactNode): void {
  root ??= createRoot(document.querySelector("#ui")!);
  root.render(<Shell>{children}</Shell>);
}
