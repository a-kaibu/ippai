// Save states over the game (web/src/savestate.ts): a small button in a corner, and the slots
// in a dialog (the screen each showed, when it was taken, save / load). The same dialog turns on
// 自動送り (web/src/autoadvance.ts); while it runs, a button beside the corner one stops it.

import { useState } from "react";
import { Bookmark, Download, FastForward, Loader2, Square, Upload } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { AUTO_INTERVALS, AUTO_MODES, type AutoMode } from "../autoadvance";
import { AUTO_SLOT } from "../savestate";
import type { AutoView, StatesView } from "./store";

const MODE_LABEL: Record<AutoMode, string> = {
  click: "クリック連打",
  enter: "Enter連打",
  ctrl: "Ctrl押しっぱなし",
};

const when = (iso: string) =>
  new Date(iso).toLocaleString("ja-JP", {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });

function AutoPanel({ auto, close }: { auto: AutoView; close: () => void }) {
  return (
    <section aria-label="自動送り" className="flex flex-col gap-2 rounded-lg border p-3">
      <div className="flex items-center justify-between gap-2">
        <h3 className="flex items-center gap-1.5 text-sm font-medium">
          <FastForward className="size-4" /> 自動送り
        </h3>
        {auto.running ? (
          <Button size="sm" variant="destructive" onClick={auto.stop}>
            <Square /> 自動送りを止める
          </Button>
        ) : (
          <Button
            size="sm"
            onClick={() => {
              auto.start();
              close();
            }}
          >
            <FastForward /> 自動送りを始める
          </Button>
        )}
      </div>
      <p className="text-xs text-muted-foreground">
        masu
        がクリックやキーを送り続けて文章を進めます。クリックはマウスを最後に置いた場所に送ります（文章の枠の上に置いてください）。選択肢では止めてください。
      </p>
      <div role="group" aria-label="送り方" className="flex flex-wrap gap-1.5">
        {AUTO_MODES.map((m) => (
          <Button
            key={m}
            size="sm"
            variant={auto.mode === m ? "default" : "outline"}
            aria-pressed={auto.mode === m}
            onClick={() => auto.set(m, auto.interval)}
          >
            {MODE_LABEL[m]}
          </Button>
        ))}
      </div>
      {auto.mode === "ctrl" ? (
        <p className="text-xs text-muted-foreground">
          Ctrl を押したままにします（キーリピートつき）。Ctrl でスキップできるゲーム向けです。
        </p>
      ) : (
        <div role="group" aria-label="速さ" className="flex flex-wrap gap-1.5">
          {AUTO_INTERVALS.map((ms) => (
            <Button
              key={ms}
              size="sm"
              variant={auto.interval === ms ? "secondary" : "ghost"}
              aria-pressed={auto.interval === ms}
              onClick={() => auto.set(auto.mode, ms)}
            >
              1秒に{Math.round(1000 / ms)}回
            </Button>
          ))}
        </div>
      )}
    </section>
  );
}

export function States({ states, auto }: { states: StatesView | null; auto: AutoView | null }) {
  const [open, setOpen] = useState(false);
  const busy = !!states && states.busy !== "";
  return (
    <>
      {auto?.running && (
        <button
          type="button"
          aria-label="自動送りを止める"
          className="fixed top-2 right-13 z-30 flex h-9 items-center gap-1.5 rounded-full bg-black/45 px-3 text-xs text-white/90 shadow backdrop-blur transition hover:bg-black/60"
          onClick={auto.stop}
        >
          <FastForward className="size-4 animate-pulse" />
          自動送り中
          <Square className="size-3" />
        </button>
      )}
      <button
        type="button"
        aria-label="どこでもセーブ"
        className="fixed top-2 right-2 z-30 grid size-9 place-items-center rounded-full bg-black/35 text-white/80 opacity-60 shadow backdrop-blur transition hover:opacity-100 focus-visible:opacity-100"
        onClick={() => {
          states?.open();
          setOpen(true);
        }}
      >
        {busy ? <Loader2 className="size-4 animate-spin" /> : <Bookmark className="size-4" />}
      </button>
      <Dialog open={open} onOpenChange={(o) => !busy && setOpen(o)}>
        <DialogContent className="max-w-2xl p-4">
          <DialogHeader>
            <DialogTitle>どこでもセーブ</DialogTitle>
            <DialogDescription>
              {!states
                ? "この実行ではメモリステートを保存できません。"
                : busy
                  ? states.busy
                  : "今の場面をそのまま保存して、あとでここから続けられます（他の端末からも）。"}
            </DialogDescription>
          </DialogHeader>
          {auto && <AutoPanel auto={auto} close={() => setOpen(false)} />}
          {states && (
            <div className="mt-2 grid grid-cols-2 gap-3">
              {states.slots.map((s, i) => (
                <div key={i} className="flex flex-col gap-2 rounded-lg border p-2">
                  <div className="grid aspect-video place-items-center overflow-hidden rounded bg-muted text-xs text-muted-foreground">
                    {s?.thumb ? (
                      <img src={s.thumb} alt="" className="size-full object-contain" />
                    ) : s ? (
                      "画面なし"
                    ) : (
                      "空き"
                    )}
                  </div>
                  <div className="flex items-center justify-between text-xs text-muted-foreground">
                    <span>スロット {i + 1}</span>
                    <span>{s ? `${when(s.created)}・${(s.size / 1048576).toFixed(0)} MB` : ""}</span>
                  </div>
                  {s?.old && <p className="text-xs text-amber-600">更新前のバージョンのステートです</p>}
                  <div className="grid grid-cols-2 gap-2">
                    <Button
                      size="sm"
                      variant="secondary"
                      disabled={busy}
                      onClick={() => {
                        if (!s || confirm(`スロット ${i + 1} に上書きしますか？`)) {
                          states.save(i);
                          setOpen(false);
                        }
                      }}
                    >
                      <Download /> セーブ
                    </Button>
                    <Button size="sm" disabled={busy || !s || s.old} onClick={() => states.load(i)}>
                      <Upload /> ロード
                    </Button>
                  </div>
                </div>
              ))}
              {states.auto && (
                <div className="col-span-2 flex items-center gap-3 rounded-lg border border-dashed p-2">
                  <div className="grid aspect-video w-28 shrink-0 place-items-center overflow-hidden rounded bg-muted text-xs text-muted-foreground">
                    {states.auto.thumb ? (
                      <img src={states.auto.thumb} alt="" className="size-full object-contain" />
                    ) : (
                      "画面なし"
                    )}
                  </div>
                  <div className="flex min-w-0 flex-1 flex-col gap-1 text-xs text-muted-foreground">
                    <span className="font-medium text-foreground">自動（画面から離れたとき）</span>
                    <span>
                      {when(states.auto.created)}・{(states.auto.size / 1048576).toFixed(0)} MB・この端末のみ
                    </span>
                    {states.auto.old && (
                      <span className="text-amber-600">更新前のバージョンのステートです</span>
                    )}
                  </div>
                  <Button size="sm" disabled={busy || states.auto.old} onClick={() => states.load(AUTO_SLOT)}>
                    <Upload /> ロード
                  </Button>
                </div>
              )}
            </div>
          )}
        </DialogContent>
      </Dialog>
    </>
  );
}
