// When the corpus' story clicks (every 2 s for STORY seconds) take a screenshot: every 20 s and
// at the end. check.py grades a title `story` only with two different story screens, which the
// default 30 s gave just one of while the shots were every 20 s alone. tests/corpus/winplay.py
// has the same rule for Windows (story_shot); tests/corpus/winplay_test.py checks both agree.
export const storyShot = (s, story) => s % 20 === 18 || s + 2 >= story;

// The title holds still once the frames of the last `need` seconds (one a second) are all the same
// screen: `need` 2 by default, a title's `title_still` column (titles.tsv) more when a logo or an
// opening stops for that long before the menu (b2023_appetite's 3 s logo, b2007_marmalade's 2 s
// stop in its opening). tests/corpus/winplay.py has the same rule (settled).
export const settled = (hashes, need) =>
  hashes.length > need && hashes.slice(-need - 1).every((h) => h === hashes[hashes.length - 1]);
// how long the wait for a still title lasts at most (25 s, more for a longer `need`)
export const stillMax = (need) => 25 + Math.max(0, need - 2) * 5;
