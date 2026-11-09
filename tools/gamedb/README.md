# The game database

`crates/ippai/src/gamedb.rs`: ErogameScape's games and VNDB's Japanese VNs in one SQLite file,
published as ghcr.io/a-kaibu/ippai-gamedb:latest (and a tag for each day, UTC). `ippai serve`
pulls it once a day into `<store>/gamedb.sqlite`.

One command makes the day's copy: it pulls the copy published now, builds a new one (keeping the
ErogameScape part of the one before when ErogameScape does not answer), checks it and pushes it:

    GHCR_TOKEN=<token with write:packages> ippai gamedb update [--egs] <work dir>

(`--egs`: ErogameScape's part must be there. The steps also run alone: `ippai gamedb pull`,
`build [--keep]`, `check [--egs]`, `push`.)

Two places run it each day:

- `.github/workflows/gamedb.yml` (19:30 UTC): VNDB's part. ErogameScape does not answer GitHub's
  runners (nor many clouds: the connection is reset or times out), so ErogameScape's part comes
  from the copy published before.
- worker2 (03:30 JST, an hour earlier): both parts, with `--egs`, from `worker2/`.

## worker2

Needs an ippai with `gamedb update` (v0.3.3 or later) at `~/.local/bin/ippai`, and a token: a
classic personal access token with `write:packages` (GitHub -> Settings -> Developer settings ->
Personal access tokens). The token stays on worker2, in a file only its user reads:

    mkdir -p ~/.config/ippai ~/.config/systemd/user
    install -m 600 /dev/null ~/.config/ippai/gamedb.env
    echo 'GHCR_TOKEN=<the token>' > ~/.config/ippai/gamedb.env
    cp tools/gamedb/worker2/ippai-gamedb.service tools/gamedb/worker2/ippai-gamedb.timer ~/.config/systemd/user/
    systemctl --user daemon-reload
    systemctl --user enable --now ippai-gamedb.timer
    loginctl enable-linger "$USER"        # runs when nobody is logged in

    systemctl --user start ippai-gamedb    # once now (ErogameScape at its pace: some minutes)
    journalctl --user -u ippai-gamedb -f

## Once

The package must be public for `ippai serve` to pull it without a token: github.com/a-kaibu ->
Packages -> ippai-gamedb -> Package settings -> Change visibility -> Public.
