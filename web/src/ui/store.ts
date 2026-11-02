// What the game pages (wine.ts, bench.ts, game.ts) show over the screen, as one small store the
// React UI (ui/Shell.tsx) renders: the status line, the start button, the cover while loading,
// &d3dstats lines and the bench panel.

import { useSyncExternalStore } from "react";

export interface BenchView {
  /** what is running, or the result */
  phase: string;
  done: boolean;
  /** the first row is the header */
  rows: string[][];
  env: string;
  /** the copy button (bench.html when done) */
  copy?: () => void;
}

export interface UiState {
  status: string;
  start: { label: string; shown: boolean; disabled: boolean; onClick?: () => void };
  /** the game's cover behind the loading messages, until it draws */
  splash: string | null;
  stats: string[];
  bench: BenchView | null;
}

let state: UiState = {
  status: "準備中",
  start: { label: "起動", shown: false, disabled: false },
  splash: null,
  stats: [],
  bench: null,
};
const listeners = new Set<() => void>();

export function getUi(): UiState {
  return state;
}

export function setUi(patch: Partial<UiState> | ((s: UiState) => Partial<UiState>)): void {
  state = { ...state, ...(typeof patch === "function" ? patch(state) : patch) };
  for (const l of listeners) l();
}

export function setStart(patch: Partial<UiState["start"]>): void {
  setUi((s) => ({ start: { ...s.start, ...patch } }));
}

export function setBench(patch: Partial<BenchView>): void {
  setUi((s) => ({ bench: { phase: "", done: false, rows: [], env: "", ...s.bench, ...patch } }));
}

export function useUi(): UiState {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => state,
  );
}
