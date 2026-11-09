// Before a game not yet on this device starts: it is saved to the device whole first (offline.ts),
// then played from there, so it never waits for the bucket mid-scene. 遊ぶ when the save is done
// (audio and fullscreen need that click); 保存せずに遊ぶ stops the save (the game's own reads get
// the line; it goes on from there next time) and streams as before. Closing the dialog leaves the
// save going (the sidebar's 保存 tab shows it).

import { useEffect, useState } from "react";
import { Play, Radio, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import type { GameEntry } from "../launcher";
import { offlineState, startSave, type OfflineState } from "../offline";
import { SaveProgress, size, useSaves } from "./Storage";

export function SaveFirst({
  game,
  onPlay,
  onClose,
}: {
  game: GameEntry;
  onPlay: (g: GameEntry) => void;
  onClose: () => void;
}) {
  const [state, setState] = useState<OfflineState | null>(null);
  const [phase, setPhase] = useState<"saving" | "done" | "failed" | "stopped">("saving");
  const [error, setError] = useState("");
  const [run, setRun] = useState(0);
  const [save, setSave] = useState<ReturnType<typeof startSave> | null>(null);
  const progress = useSaves().get(game.id);
  useEffect(() => {
    let live = true;
    void offlineState(game).then(
      (s) => live && setState(s),
      () => {},
    );
    const s = startSave(game);
    setSave(s);
    setPhase("saving");
    setError("");
    s.done.then(
      () => live && setPhase("done"),
      (e: unknown) => {
        if (!live) return;
        setPhase(s.stop.signal.aborted ? "stopped" : "failed");
        if (!s.stop.signal.aborted) setError(String(e instanceof Error ? e.message : e));
      },
    );
    return () => void (live = false);
  }, [game, run]);
  const stream = () => {
    save?.stop.abort();
    onPlay(game);
  };
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-md p-5" data-testid="save-first">
        <DialogHeader className="pr-8">
          <DialogTitle>{game.title}</DialogTitle>
          <DialogDescription>
            {phase === "done"
              ? "端末に保存しました。読み込みで止まらずに遊べます。"
              : `読み込みで止まらないように、ゲームを丸ごとこの端末に保存してから遊びます${state ? `（約 ${size(state.bytes)}）` : ""}。次からはすぐに始まります。`}
          </DialogDescription>
        </DialogHeader>
        <div className="mt-3 grid gap-3 text-sm">
          {phase === "saving" &&
            (progress ? (
              <SaveProgress p={progress} />
            ) : (
              <p className="text-xs text-muted-foreground">保存の準備中…</p>
            ))}
          {phase === "stopped" && <p className="text-muted-foreground">保存を中止しました。</p>}
          {error && <p className="text-destructive">保存できませんでした: {error}</p>}
          <div className="flex flex-wrap gap-2">
            {phase === "done" ? (
              <Button className="flex-1" onClick={() => onPlay(game)} autoFocus>
                <Play className="fill-current" /> 遊ぶ
              </Button>
            ) : (
              <>
                {phase === "saving" ? (
                  <Button variant="outline" onClick={() => save?.stop.abort()}>
                    <X /> 中止
                  </Button>
                ) : (
                  <Button onClick={() => setRun((n) => n + 1)}>続きから保存</Button>
                )}
                <Button variant="ghost" onClick={stream} title="今までどおり、遊びながら読み込みます">
                  <Radio /> 保存せずに遊ぶ
                </Button>
              </>
            )}
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
