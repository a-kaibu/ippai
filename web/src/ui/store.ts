// What the game pages (play.ts, bench.ts, game.ts) show over the screen, as one small store the
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
  /** a "back to the list" button (after a crash or an error) */
  back: (() => void) | null;
  /** debug mode: network, memory and fps over the game (ui/Debug.tsx) */
  debug: boolean;
  /** requests are being retried: the link dropped or stalls (web/src/net.ts) */
  net: boolean;
  /** save states of the running game (ui/States.tsx, web/src/savestate.ts); null: none here */
  states: StatesView | null;
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

const DEBUG = "vnvm.debug";

/**
 * Debug mode: the list's switch (kept in localStorage), ?debugmode=1, Ctrl+Shift+D or a
 * three-finger tap over the game (play.ts).
 */
function debugAtStart(): boolean {
  const q = new URLSearchParams(location.search).get("debugmode");
  if (q !== null) return q !== "0";
  try {
    return localStorage.getItem(DEBUG) === "1";
  } catch {
    return false;
  }
}

let state: UiState = {
  status: "準備中",
  start: { label: "起動", shown: false, disabled: false },
  splash: null,
  stats: [],
  bench: null,
  back: null,
  debug: debugAtStart(),
  net: false,
  states: null,
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

export function setDebug(on: boolean): void {
  try {
    localStorage.setItem(DEBUG, on ? "1" : "0");
  } catch {
    // (no storage: this page only)
  }
  setUi({ debug: on });
}

addEventListener(
  "keydown",
  (e) => {
    if (e.ctrlKey && e.shiftKey && e.code === "KeyD") {
      e.preventDefault();
      e.stopImmediatePropagation();
      setDebug(!state.debug);
    }
  },
  true,
);
