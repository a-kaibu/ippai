# The game database

`crates/ippai/src/gamedb.rs`: ErogameScape's games and VNDB's Japanese VNs in one SQLite file,
published as ghcr.io/a-kaibu/ippai-gamedb:latest (and a tag for each day, UTC). `ippai serve`
pulls it once a day into `<store>/gamedb.sqlite`.

One command makes the day's copy: it pulls the copy published now, builds a new one (keeping the
ErogameScape part of the one before when ErogameScape does not answer), checks it and pushes it:

    GHCR_TOKEN=<token with write:packages> ippai gamedb update [--egs] <work dir>

(`--egs`: ErogameScape's part must be there. The steps also run alone: `ippai gamedb pull`,
`build [--keep]`, `check [--egs]`, `push`.)

`.github/workflows/gamedb.yml` runs it each day at 03:30 JST on worker2 as a self-hosted runner:
ErogameScape refuses GitHub's own runners (and many clouds) by address, and answers worker2's.
The job takes ippai from the latest release and the job's own token for the push, so worker2
needs no toolchain and no token of its own (curl, tar and python3 only).

## worker2 as the runner (once)

1. github.com/a-kaibu/ippai -> Settings -> Actions -> Runners -> New self-hosted runner -> Linux:
   run the download and `./config.sh --url ... --token ...` lines it shows on worker2 (in
   `~/actions-runner`), adding `--labels worker2` (or type `worker2` when it asks for labels).
2. As a service, so it runs after reboots: `sudo ./svc.sh install && sudo ./svc.sh start`.
3. Settings -> Actions -> General -> "Approval for running fork pull request workflows from
   contributors": "Require approval for all external contributors", so no one else's pull
   request runs on worker2 (only gamedb.yml asks for it).
4. Actions -> Game database -> Run workflow, once now.

## Once

The package must be public for `ippai serve` to pull it without a token: github.com/a-kaibu ->
Packages -> ippai-gamedb -> Package settings -> Change visibility -> Public.
