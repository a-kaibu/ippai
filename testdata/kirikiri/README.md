# 吉里吉里ベンチ

A small visual novel for 吉里吉里Z (krkrz) and KAG3, made for masu's benchmarks
(`tests/bench`, group `kirikiri`: `pack-kirikiri.mjs` packs this folder into data.xp3 next to
krkrz 1.4.0r2's tvpwin32.exe, `scenario.mjs` plays it). The story and the harness's signals
are in `scenario/first.ks`, the heavy effects in `bench.tjs`.

- `system/`, `startup.tjs`: KAG3 for 吉里吉里Z (krkrz/kag3 1f3ab30, "改変・配布は自由"),
  `Config.tjs` changed (1280x720, no menu bar, save thumbnails, positions and font size), `Conductor.tjs`
  logs no tags (as released games).
- Pictures (`bgimage/`, `fgimage/`, `rule/`, `image/`) and sounds (`sound/`, `bgm/`) were made
  procedurally for this game (backgrounds and characters with noise, a diagonal wipe rule,
  tones for voices and BGM).
