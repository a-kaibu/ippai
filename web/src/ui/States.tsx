// Save states over the game (web/src/savestate.ts): a small button in a corner, and the slots
// in a dialog (the screen each showed, when it was taken, save / load).

import { useState } from "react";
import { Bookmark, Download, Loader2, Upload } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import type { StatesView } from "./store";

const when = (iso: string) =>
  new Date(iso).toLocaleString("ja-JP", {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });

export function States({ states }: { states: StatesView }) {
  const [open, setOpen] = useState(false);
  const busy = states.busy !== "";
  return (
    <>
      <button
        type="button"
        aria-label="どこでもセーブ"
        className="fixed top-2 right-2 z-30 grid size-9 place-items-center rounded-full bg-black/35 text-white/80 opacity-60 shadow backdrop-blur transition hover:opacity-100 focus-visible:opacity-100"
        onClick={() => {
          states.open();
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
              {busy
                ? states.busy
                : "今の場面をそのまま保存して、あとでここから続けられます（他の端末からも）。"}
            </DialogDescription>
          </DialogHeader>
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
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}
