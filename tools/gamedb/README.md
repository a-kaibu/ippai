# The game database

`crates/ippai/src/gamedb.rs`: ErogameScape's games and VNDB's Japanese VNs in one SQLite file,
published as ghcr.io/a-kaibu/ippai-gamedb:latest (and a tag for the day, UTC). `ippai serve`
looks for a new copy once a day and keeps it as `<store>/gamedb.sqlite`, however old it is.

It is rebuilt by hand, now and then (the user, 2026-11-09: "一旦手動更新でいいや"). One command
pulls the copy published now, builds a new one (keeping the ErogameScape part of the one before
when ErogameScape does not answer), checks it and pushes it:

    tools/gamedb/update.sh [work dir]

(the GitHub CLI's sign-in for the token: it signs in or adds the write:packages scope when
needed, and takes ippai from the latest release unless one with `gamedb update` is on PATH or in
$IPPAI; Linux, or Git Bash on Windows). Without the script:

    GHCR_TOKEN=<token with write:packages> ippai gamedb update --egs <work dir>

(`ippai gamedb push`/`update` also take `gh auth token` when GHCR_TOKEN is not set.) Run it on a host ErogameScape answers (worker2, a home PC: it refuses GitHub's runners and many
clouds by address). `--egs` refuses to push without ErogameScape's part. It takes some minutes
(ErogameScape at its pace, VNDB within its rate limit). A token of your own instead of gh's: a
classic personal access token with `write:packages`, kept on that host only.

VNDB's part alone: Actions -> Game database -> Run workflow (`.github/workflows/gamedb.yml`, the
job's own token; ErogameScape's part kept from the copy before).

The steps also run alone: `ippai gamedb pull`, `build [--keep]`, `check [--egs]`, `push`.

## Once

The package must be public for `ippai serve` to pull it without a token: github.com/a-kaibu ->
Packages -> ippai-gamedb -> Package settings -> Change visibility -> Public.
