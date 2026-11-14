// What the game pages (play.ts, bench.ts, game.ts) show over the screen, as one small store the
// React UI (ui/Shell.tsx) renders: the status line, the start button, the cover while loading,
// &d3dstats lines and the bench panel.

import { useSyncExternalStore } from "react";
import type { AutoMode } from "../autoadvance";

export interface BenchView {
  /** what is running, or the result */
  phase: string;
  done: boolean;
  /** the first row is the header */
  rows: string[][];
  env: string;
  /** the copy button (bench.html when done) */
  copy?: () => void;
  /** buttons to pick from (bench.html's profiles) */
  choices?: { label: string; detail: string; onClick: () => void }[];
}

export interface UiState {
  status: string;
  start: { label: string; shown: boolean; disabled: boolean; onClick?: () => void };
  /** the game's cover behind the loading messages, until it draws */
  splash: string | null;
  stats: string[];
  bench: BenchView | null;
  /** a "back to the list" button (after a crash or an error) */
  back: (() => void) | null;
  /** requests are being retried: the link dropped or stalls (web/src/net.ts) */
  net: boolean;
  /** save states of the running game (ui/States.tsx, web/src/savestate.ts); null: none here */
  states: StatesView | null;
  /** 自動送り (web/src/autoadvance.ts), in the same dialog; null: no game running */
  auto: AutoView | null;
}

export interface AutoView {
  mode: AutoMode;
  interval: number;
  running: boolean;
  /** the mode and the interval chosen (kept for the next game; a running one goes on in them) */
  set: (mode: AutoMode, interval: number) => void;
  start: () => void;
  stop: () => void;
}

export interface StatesView {
  /** the slots (null: empty), as the dialog shows them */
  slots: ({ created: string; thumb: string; size: number; old: boolean } | null)[];
  /** what is being done ("": nothing) */
  busy: string;
  /** opens the dialog (reads the slots again) */
  open: () => void;
  save: (slot: number) => void;
  load: (slot: number) => void;
}

let state: UiState = {
  status: "準備中",
  start: { label: "起動", shown: false, disabled: false },
  splash: null,
  stats: [],
  bench: null,
  back: null,
  net: false,
  states: null,
  auto: null,
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
