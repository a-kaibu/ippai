# The game database

`crates/ippai/src/gamedb.rs`: ErogameScape's games and VNDB's Japanese VNs in one SQLite file,
published as ghcr.io/a-kaibu/ippai-gamedb:latest (`.github/workflows/gamedb.yml`, daily).

ErogameScape does not answer GitHub's runners (nor many clouds: the connection is reset or
times out). The workflow then keeps the ErogameScape part of the copy published before, so that
part has to be built once (and refreshed now and then) on a host it answers:

    ippai gamedb pull before.sqlite              # what is published now (VNDB's part, maybe more)
    ippai gamedb build --keep before.sqlite gamedb.sqlite
    GHCR_TOKEN=<token with write:packages> python3 tools/gamedb/push.py gamedb.sqlite

The package must be public for `ippai serve` to pull it without a token (Package settings ->
Change visibility, once).
