import { expect, test } from "vite-plus/test";
import { Held } from "./held";

/**
 * Random presses and releases of buttons and keys, with cancels (pointercancel: the buttons) and
 * blurs (everything) among them, against a model of what the game was told: after each blur
 * nothing is held for the game (every button and key it was told is down gets its release, the
 * key with the data it was given), after a cancel no button is, keys stay (property, seeded).
 */
test("what the game holds is let go at a cancel (buttons) and a blur (all) (property, seeded)", () => {
  let x = 19;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  const codes = ["ControlLeft", "KeyA", "Enter", "Space"];
  for (let run = 0; run < 500; run++) {
    const h = new Held();
    const game = { buttons: new Set<number>(), keys: new Map<string, number>() };
    for (let step = 0; step < 40; step++) {
      const op = rnd(6);
      if (op === 0) {
        const b = rnd(3);
        h.button(b, true);
        game.buttons.add(b);
      } else if (op === 1) {
        const b = rnd(3);
        h.button(b, false);
        game.buttons.delete(b);
      } else if (op === 2) {
        const c = codes[rnd(4)]!;
        const d = rnd(1000);
        h.key(c, true, d);
        game.keys.set(c, d);
      } else if (op === 3) {
        const c = codes[rnd(4)]!;
        h.key(c, false);
        game.keys.delete(c);
      } else if (op === 4) {
        const ups = h.releaseButtons();
        expect(new Set(ups), `run ${run} step ${step}`).toEqual(game.buttons);
        expect(ups.length).toBe(game.buttons.size);
        game.buttons.clear();
      } else {
        const r = h.release();
        expect(new Set(r.buttons)).toEqual(game.buttons);
        const num = (a: number, b: number) => a - b;
        expect(r.keys.sort(num)).toEqual([...game.keys.values()].sort(num));
        game.buttons.clear();
        game.keys.clear();
        expect(h.size).toBe(0);
      }
      expect(h.size).toBe(game.buttons.size + game.keys.size);
    }
  }
});
