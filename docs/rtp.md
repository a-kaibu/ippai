# RPG Maker RTPs (#155, #232)

Games made with RPG Maker leave the engine's shared material (pictures, sounds, fonts and, for
some versions, the engine DLL) out of their own folder and read it from the run time package
(RTP) installed on the PC. Without it RGSS3 (VX Ace) stops with 「このゲームの実行には RPGVXAce
RTP が必要です」 and RGSS2 (VX) with 「RPGVX RTP が見つかりません」 or `RGSS202J.dll could not be
found`, on Windows as on sake.

## The official RTPs: downloaded by each server, never redistributed

The official RTPs are free to download (https://rpgmakerofficial.com/support/rtp/, files on
cdn.tkool.jp), but not to pass on. The page and each package's 利用規約.txt (read 2026-11-13):

- Support page (all RTPs): 「RTPは株式会社Gotcha Gotcha Gamesの文書による許諾なく、データの複製
  や、営利、非営利を問わず２次配布を禁止します」, and the RTP may only be used to make games with
  a registered copy of that RPG Maker or to play games made with it.
- VX Ace RTP 1.00 (`vxace_rtp100.zip`, license of 2011-12-15): 「ユーザーは、本RTPの全部又は一部を
  配布、販売、加工・改変等することはできません」.
- VX RTP 2.02 (`vx_rtp202.zip`): no redistribution without Enterbrain's written permission.

So no RTP file is in the repository, in Releases or in any CI artifact. Each masu server
downloads the packages from the publisher itself, as a player installing them on Windows
would, and installs them for its own sake (user's call, 2026-11-14: 「サーバーが公式から自動取得」).
An earlier stand-in (our own generated pictures and tones under the RTP's names) was replaced
by this.

## crates/masu/src/rtp.rs

`rtp::PACKAGES` lists the five official packages with their SHA-256 and size; a download that
does not match is not used.

| package | setup | installed at (sake's C:\) | registry (sake misc.rs RTPS) |
| --- | --- | --- | --- |
| VX Ace `vxace_rtp100.zip` | Inno Setup 5.4.2 (Unicode) | `Program Files\Common Files\Enterbrain\RGSS3\RPGVXAce` | `HKLM\Software\Enterbrain\RGSS3\RTP` `RPGVXAce` |
| VX `vx_rtp202.zip` | Inno Setup 5.3.10 | `...\Enterbrain\RGSS2\RPGVX` (with the engine, RGSS202J.dll) | `HKLM\Software\Enterbrain\RGSS2\RTP` `RPGVX` |
| XP `xp_rtp103.zip` | Inno Setup | `...\Enterbrain\RGSS\Standard`, RGSS100J/102E/102J/103J.dll in `windows\system32` | `HKLM\Software\Enterbrain\RGSS\RTP` `Standard` |
| 2000 `2000rtp.zip` (+ `RTP_patchk_190612.zip`) | InstallShield 6 in a cabinet self-extractor | `Program Files\ASCII\RPG2000\RTP` | `HKLM` and `HKCU\Software\ASCII\RPG2000` `RuntimePackagePath` |
| 2003 `2003rtp.zip` | InstallShield 6 in a cabinet self-extractor | `Program Files\Enterbrain\RPG2003\RTP` | `HKLM` and `HKCU\Software\Enterbrain\RPG2003` `RuntimePackagePath` |

The setups are read, not run:

- Inno Setup: the `inno` crate (0.7.5) gives the setup's default folder and its files with
  their destinations (`{app}`, `{cf}`, `{sys}`; `rtp::map_path` turns them into drive_c paths
  for a 32-bit Windows and refuses anything that would leave drive_c).
- InstallShield: the `cab` crate takes data1.hdr / data1.cab / data2.cab out of the
  self-extractor, and `iscab.rs` reads the InstallShield 6 cabinet (file table, split volumes,
  obfuscation, deflate chunks, each file's MD5 checked). The files of the setup's `RTP` folder go
  to the install folder.
- 2000: the publisher's fixed pictures (`RTP_patchk_190612.zip`, the support page's
  instructions) replace the setup's モンスター / 主人公3 / 基本 / ダンジョン / 船 / 内装 / 外観.

`masu rtp [--out DIR] [--cache DIR]` installs them into DIR (default the data directory's
`rtp/`) as `drive_c/...` with an `INSTALLED` list; downloads (~280 MB) stay in the cache's
`rtp/`. `masu serve` does it in a background thread when the data directory has not got all
five (env `MASU_RTP=0`: not), then puts them in sake's C:\.

## Into sake's C:\

`pack::ensure_prefix(store, fonts, Some(rtp))` (at serve's start, after the background install,
and `masu prefix FONTS --rtp DIR`) copies the RTP part of `rtp/drive_c` (`pack::RTP_DIRS` and
the XP RTP's system32 DLLs) into the tree `prefix32`, with a stamp
`Program Files/masu-rtp-<hash of those files>`. A tree with another stamp gets its RTP part
replaced and keeps the rest (fonts, files put in by hand: `merge_rtp`). sake sets each RTP's
registry value when its folder is there.

## Tests

- `rtp::tests::official_packages` (`MASU_RTP_TEST=<cache>`; CI caches the zips): all five
  installed, files where the setups put them; with `MASU_RTP_ORACLE=1` every file compared
  byte for byte with innoextract (Inno) and unshield (InstallShield) output.
- proptests: `rtp::paths_stay_inside` (setup paths never leave drive_c),
  `iscab::garbage_is_an_error`, `iscab::deobfuscate_is_one_to_one`,
  `pack::merge_rtp_replaces_the_rtp_part`; `pack::prefix_with_rtps`.
- Corpus (`tests/corpus/ci.sh` with `RTP=`, corpus.yml runs `masu rtp` with the cached zips):
  VX Ace dl_RJ260990 / dl_RJ177152 / dl_RJ227621, VX dl_RJ106544 (ships its RGSS202J.dll) and
  dl_RJ170671 (the engine from the RTP).

Found on the way: sake's WriteConsoleA/W took any handle, and Ruby 1.9 tries WriteConsoleW
with 0 characters on every file before writing it with WriteFile, so RGSS3 games' Marshal.dump
(save files, a game's own system data) went to the log and left the files empty
(堕落の淫蕩吸血鬼: 「EOFError が発生しました」). They now fail with ERROR_INVALID_HANDLE on
anything but the console's output (tests/winref/probes/writecon.c).

RGSS (1 to 3) draws sprites with blend loops it writes as machine code into one buffer, the
plain and the zooming one taking turns there many times a frame. The JIT used to give that
page to the interpreter after 256 rewrites, and ヴァンパイア・ノーツ's title (a zoomed
background) ran at under 1 frame a second, too slow to see a key pressed for less than a frame
(#233). Each thread now keeps the translations of the versions it has seen and puts one back
when its code returns (crates/kura/src/jit/park.rs, host test codeturns): about 13 frames a
second under node.
