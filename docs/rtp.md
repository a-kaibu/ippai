# RPG Maker RTPs (#155)

Games made with RPG Maker leave the engine's shared material (pictures, sounds, fonts and, for
some versions, the engine DLL) out of their own folder and read it from the run time package
(RTP) installed on the PC. Without it RGSS3 (VX Ace) stops with 「このゲームの実行には RPGVXAce
RTP が必要です」 and RGSS2 (VX) with 「RPGVX RTP が見つかりません」 or `RGSS202J.dll could not be
found`, on Windows as on sake.

## The official RTPs may not be redistributed

The official RTPs are free to download (https://rpgmakerofficial.com/support/rtp/, files on
cdn.tkool.jp), but not to pass on. The page and each package's 利用規約.txt (read 2026-11-13):

- Support page (all RTPs): 「RTPは株式会社Gotcha Gotcha Gamesの文書による許諾なく、データの複製
  や、営利、非営利を問わず２次配布を禁止します」, and the RTP may only be used to make games with
  a registered copy of that RPG Maker or to play games made with it.
- VX Ace RTP 1.00 (`vxace_rtp100.zip`, license of 2011-12-15): 「ユーザーは、本RTPの全部又は一部を
  配布、販売、加工・改変等することはできません」; no reverse engineering.
- VX RTP 2.02 (`vx_rtp202.zip`): no redistribution without Enterbrain's written permission; no
  reverse engineering.

So ippai ships none of the official RTPs' files: not in the repository, not in Releases.

## ippai's own RTPs

Like sake's fonts (IPA fonts under the names of the Windows fonts), ippai makes its own stand-in
for each RTP (user's call, 2026-11-13: 「再配布じゃなくて再実装」):

- `tools/rtp/<rtp>.tsv`: the paths games ask for, and each picture's width and height (the
  engine cuts character sheets, faces, icons, tilesets and the window skin by size). Only names
  and sizes, taken from the official installer's file list.
- `tools/ci/sake-rtp.py OUT`: makes `OUT/<rtp>/` with files of those names: pictures drawn in the
  layout the engine reads (8 characters x 3 frames x 4 directions, 96x96 faces, 24x24 icons with
  icon 0 empty, tileset B's first tile empty, the window skin's back / frame / cursor / pause
  sign / 32 text colors), and short Ogg Vorbis tones (BGM and BGS loops, ME phrases, SE blips).
  The same input makes the same bytes. Fonts that are free to redistribute go in as they are
  (VX Ace: VL Gothic, from Debian's fonts-vlgothic, with its license).

A game that draws or plays RTP material runs, with our pictures and sounds in place of the
official ones; what the game ships itself is unchanged.

## How it gets to sake's C:\

- Release (`.github/workflows/release.yml`): `sake-rtp.py fonts/rtp` next to `sake-fonts.py
  fonts`, so the archives carry `fonts/rtp/<rtp>/` and install.sh / `ippai update` put it with
  the fonts in the data directory.
- `ippai serve` (`pack::ensure_prefix`, also `ippai prefix FONTS`): copies each RTP into the
  tree `prefix32` (sake's C:\) at the folder its official installer uses on a 32-bit Windows,
  `C:\Program Files\Common Files\Enterbrain\RGSS3\RPGVXAce` for VX Ace (`pack::RTPS`). The tree
  carries a stamp (`Enterbrain/ippai-rtp-<hash of the RTPs' files>`): a newer release with other
  RTP files replaces the RTP part of the tree and keeps the rest (fonts, files put in by hand).
- sake (`crates/sake/src/misc.rs` RTPS): when the folder is there, the registry has what the
  installer writes, `HKLM\SOFTWARE\Enterbrain\RGSS3\RTP` value `RPGVXAce` = the folder.
- Corpus (`tests/corpus/ci.sh`): the base store is made by `ippai prefix` from the fonts and the
  RTPs, as a server makes it.

## Status

| RTP | engine | state |
| --- | --- | --- |
| VX Ace (RGSS3) | game ships `System\RGSS301.dll` | ours: 747 files + VL Gothic, ~13 MB. Corpus: dl_RJ260990 story, dl_RJ177152 story text (one still frame), dl_RJ227621 title (its title takes no keys on sake: own issue) |
| VX (RGSS2) | `RGSS202J.dll` is in the RTP, not in games | not yet: needs our own RGSS2 DLL (Ruby 1.8 + RGSS2) besides the material |
| XP (RGSS1), 2000, 2003 | XP games ship RGSS10xJ.dll; 2000/2003 have the engine in RPG_RT.exe | not yet: a manifest and the registry keys when a corpus title needs them |

Found on the way: sake's WriteConsoleA/W took any handle, and Ruby 1.9 tries WriteConsoleW
with 0 characters on every file before writing it with WriteFile, so RGSS3 games' Marshal.dump
(save files, a game's own system data) went to the log and left the files empty
(堕落の淫蕩吸血鬼: 「EOFError が発生しました」). They now fail with ERROR_INVALID_HANDLE on
anything but the console's output (tests/winref/probes/writecon.c).
