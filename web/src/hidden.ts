// A game out of sight (another tab, another app, the screen off): it stands still until it is
// seen again, and a state of it is kept in this browser first (the slot AUTO_SLOT,
// savestate.ts), so that a tab the browser discarded meanwhile (Android does, often) goes on
// from there when it is opened again, not from the game's last save. Before that, the files
// the game wrote are put away (saves.ts). Standing still costs nothing: every thread waits,
// the mixer too, and the program's clocks don't move (crates/kura/src/snap.rs CMD_PAUSE), so
// its timings go on as they were.
//
// visibilitychange is what says it (it comes first, also before pagehide and freeze); freeze
// only in case it did not. pagehide is a page going away (a reload, the list): nothing to keep
// then but the saves.

import type { Stopped } from "./savestate";

export interface HideDeps {
  /** Whether the page is out of sight now. */
  hidden(): boolean;
  /** The game's own save files put away now. */
  flushSaves(): Promise<void>;
  /** Whether this run's states may be kept (one process, no secrets in its memory). */
  canState(): boolean;
  /** A state taken and kept, the program then standing (`kept` false: a thread ran while it
   * was copied, none kept). */
  take(): Promise<{ stopped: Stopped; kept: boolean }>;
  /** The program standing still, nothing kept. */
  pause(): Promise<Stopped>;
  now(): number;
  log(line: string): void;
}

/** A state is taken again only when the program ran this long since the last one (switching
 * apps back and forth: nothing new to keep, and the way back shows no wait). */
export const MIN_RUN_MS = 5000;

export class HideKeeper {
  private chain: Promise<void> = Promise.resolve();
  private held: Stopped | null = null;
  /** ms the program ran since the last state kept (Infinity: none kept in this run) */
  private ran = Infinity;
  private since: number;
  private leaving = false;
  /** (tests) states kept, pauses */
  kept = 0;
  paused = 0;

  constructor(private d: HideDeps) {
    this.since = d.now();
  }

  /** Out of sight (visibilitychange, freeze). */
  hide(): Promise<void> {
    return (this.chain = this.chain.then(() => this.stop()));
  }
  /** Seen again (visibilitychange, resume, pageshow). */
  show(): Promise<void> {
    this.leaving = false;
    return (this.chain = this.chain.then(() => this.go()));
  }
  /** The page goes away (pagehide): no state, the saves only. */
  leave(): void {
    this.leaving = true;
  }
  /** Whether the program stands still now. */
  get standing(): boolean {
    return this.held !== null;
  }

  private async stop(): Promise<void> {
    if (this.held || !this.d.hidden()) return;
    // (first: the files are the game's own, a state only a way back to them)
    await this.d.flushSaves().catch((e: unknown) => this.d.log(`hidden: saves: ${String(e)}`));
    if (!this.d.hidden() || this.leaving) return;
    const ran = this.ran + (this.d.now() - this.since);
    if (this.d.canState() && ran >= MIN_RUN_MS) {
      try {
        const { stopped, kept } = await this.d.take();
        this.held = stopped;
        if (kept) {
          this.ran = 0;
          this.kept++;
        } else {
          this.ran = ran;
          this.d.log("hidden: a thread ran while copied, no state kept");
        }
        this.paused++;
        return;
      } catch (e) {
        this.d.log(`hidden: no state: ${e instanceof Error ? e.message : String(e)}`);
      }
      if (!this.d.hidden()) return;
    }
    try {
      this.held = await this.d.pause();
      this.ran = ran;
      this.paused++;
    } catch (e) {
      this.d.log(`hidden: not paused: ${e instanceof Error ? e.message : String(e)}`);
      this.ran = ran;
      this.since = this.d.now();
    }
  }

  private async go(): Promise<void> {
    if (!this.held || this.d.hidden()) return;
    const h = this.held;
    this.held = null;
    await h.resume().catch((e: unknown) => this.d.log(`hidden: resume: ${String(e)}`));
    this.since = this.d.now();
  }
}

/** The keeper on the page's own events. */
export function keepWhileHidden(d: Omit<HideDeps, "hidden" | "now">): HideKeeper {
  const k = new HideKeeper({
    ...d,
    hidden: () => document.visibilityState === "hidden",
    now: () => performance.now(),
  });
  document.addEventListener(
    "visibilitychange",
    () => void (document.visibilityState === "hidden" ? k.hide() : k.show()),
  );
  document.addEventListener("freeze", () => void k.hide());
  document.addEventListener("resume", () => void k.show());
  addEventListener("pagehide", () => k.leave());
  addEventListener("pageshow", () => void k.show());
  if (document.visibilityState === "hidden") void k.hide();
  return k;
}
