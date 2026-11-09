# The game database

`crates/ippai/src/gamedb.rs`: ErogameScape's games and VNDB's Japanese VNs in one SQLite file,
published as ghcr.io/a-kaibu/ippai-gamedb:latest (and a tag for the day, UTC). `ippai serve`
looks for a new copy once a day and keeps it as `<store>/gamedb.sqlite`, however old it is.

It is rebuilt by hand, now and then (the user, 2026-11-09: "一旦手動更新でいいや"), each part
where it can be made. `ippai gamedb update` pulls the copy published now, makes one part anew and
keeps the other from that copy, checks the result and pushes it:

- ErogameScape's part: `tools/gamedb/update.sh [work dir]` on a host ErogameScape answers
  (worker2, a home PC; it refuses GitHub's runners and many clouds by address). It runs
  `ippai gamedb update --egs` with the GitHub CLI's sign-in for the token (it signs in or adds
  the write:packages scope when needed) and ippai from the latest release (unless one with
  `gamedb update` is on PATH or in $IPPAI); Linux, or Git Bash on Windows. Some minutes.
- VNDB's part: Actions -> Game database -> Run workflow (`.github/workflows/gamedb.yml`:
  `ippai gamedb update`, the job's own token; ErogameScape not asked). About half an hour (VNDB's
  rate limit: 429s are waited out and the page asked again).

Either fetches the other part too when the copy before has none of it. Without the script:
`GHCR_TOKEN=<token with write:packages> ippai gamedb update [--egs] <work dir>` (GHCR_TOKEN unset:
`gh auth token`). The steps alone: `ippai gamedb pull`, `build [--keep]` (both parts),
`check [--egs]`, `push`.

## Once

The package must be public for `ippai serve` to pull it without a token: github.com/a-kaibu ->
Packages -> ippai-gamedb -> Package settings -> Change visibility -> Public.
