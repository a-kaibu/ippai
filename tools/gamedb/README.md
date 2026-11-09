# The game database

`crates/ippai/src/gamedb.rs`: ErogameScape's games and VNDB's Japanese VNs, each part its own
SQLite file, published as ghcr.io/a-kaibu/ippai-gamedb with a tag for each part (`egs`, `vndb`)
and for each part and day (`egs-20261109`). `ippai serve` looks for new copies once a day and
keeps them as `<store>/gamedb-egs.sqlite` and `<store>/gamedb-vndb.sqlite`, however old.

Each part is rebuilt by hand, now and then, where it can be made (the user, 2026-11-09:
"一旦手動更新でいいや", "批評空間とvndbでdb分けた方がいい"):

- ErogameScape's: `tools/gamedb/update.sh [work dir]` on a host ErogameScape answers (worker2,
  a home PC; it refuses GitHub's runners and many clouds by address). It runs
  `ippai gamedb update --part egs` with the GitHub CLI's sign-in for the token (it signs in or
  adds the write:packages scope when needed) and ippai from the latest release (unless one with
  `gamedb update --part` is on PATH or in $IPPAI); Linux, or Git Bash on Windows. Some minutes.
- VNDB's: Actions -> Game database -> Run workflow (`.github/workflows/gamedb.yml`,
  `ippai gamedb update --part vndb`, the job's own token). About half an hour (VNDB's rate
  limit: a 429 is waited out, longer each time, and the page asked again).

A part is pushed (and taken by the server) only when it holds enough (`ippai gamedb check`:
over 20000 PC games with 30345 = LOOPERS; over 10000 VNs); one failing leaves the published one.
Without the script: `GHCR_TOKEN=<token with write:packages> ippai gamedb update --part <part> <dir>`
(GHCR_TOKEN unset: `gh auth token`). The steps alone: `ippai gamedb build|pull|check|push --part`.

The package must be public for `ippai serve` to pull it without a token (it is).
