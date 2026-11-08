// The page as an installed app (manifest.webmanifest) that opens offline (sw.ts): registers the
// service worker, keeps the browser's install prompt for the library's button, and tells the
// library when a new build took over (it then offers a reload; a running game is left alone).

import { useSyncExternalStore } from "react";

interface InstallPrompt extends Event {
  prompt(): Promise<void>;
  userChoice: Promise<{ outcome: "accepted" | "dismissed" }>;
}

export interface AppState {
  /** the browser lets the page install itself (Chrome's prompt, kept for a click) */
  install: (() => void) | null;
  /** a new build took over since this page loaded */
  updated: boolean;
  /** the browser has a network and the server answered (sw.ts) */
  online: boolean;
}

let state: AppState = { install: null, updated: false, online: navigator.onLine };
const listeners = new Set<() => void>();
function set(s: Partial<AppState>): void {
  state = { ...state, ...s };
  for (const l of listeners) l();
}

export function useApp(): AppState {
  return useSyncExternalStore(
    (l) => (listeners.add(l), () => listeners.delete(l)),
    () => state,
  );
}

/**
 * Not in dev (vp dev serves sources) nor in headless runs (tests, benchmarks, ippai serve's
 * pre-runs: they measure the network as it is).
 */
export function startApp(): void {
  addEventListener("online", () => void checkOnline());
  addEventListener("offline", () => set({ online: false }));
  addEventListener("beforeinstallprompt", (e) => {
    e.preventDefault();
    const p = e as InstallPrompt;
    set({
      install: () => {
        void p.prompt();
        void p.userChoice.finally(() => set({ install: null }));
      },
    });
  });
  addEventListener("appinstalled", () => set({ install: null }));
  const params = new URLSearchParams(location.search);
  // (a kept app of an older build, crates/ippai/src/apps.rs: the root's worker is the app's)
  const kept = /\/apps\/[0-9a-f]{16}\/[^/]*$/.test(location.pathname);
  if (!import.meta.env.PROD || !("serviceWorker" in navigator) || params.has("headless") || kept) return;
  const had = !!navigator.serviceWorker.controller;
  navigator.serviceWorker.addEventListener("controllerchange", () => had && set({ updated: true }));
  navigator.serviceWorker
    .register("sw.js")
    .then((r) => addEventListener("online", () => void r.update().catch(() => undefined)))
    .catch((e: unknown) => console.warn(`service worker: ${String(e)}`));
}

/**
 * Whether the page is online: the browser's word, and the service worker's (a phone with a
 * network but the server out of reach: the page came from the device). The library asks once
 * it has the list.
 */
export async function checkOnline(): Promise<void> {
  const w = navigator.serviceWorker?.controller;
  if (!navigator.onLine || !w) return set({ online: navigator.onLine });
  const ch = new MessageChannel();
  const answer = new Promise<{ unreachable: boolean }>((resolve) => {
    ch.port1.onmessage = (e: MessageEvent<{ unreachable: boolean }>) => resolve(e.data);
    setTimeout(() => resolve({ unreachable: false }), 1000);
  });
  w.postMessage("state", [ch.port2]);
  set({ online: !(await answer).unreachable });
}
