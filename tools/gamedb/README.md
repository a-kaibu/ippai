# The game database

`crates/ippai/src/gamedb.rs` publishes ErogameScape games and Japanese-language VNDB VNs as
separate compact SQLite files in `ghcr.io/a-kaibu/ippai-gamedb`, tagged `egs` / `vndb` and
by date (e.g. `egs-20261112`). Servers keep `<store>/gamedb-{egs,vndb}.sqlite` and check for
new published copies once a day. Runtime queries and the distribution format are unchanged.

Builds now use the public [vn-archive SQLite mirrors](https://huggingface.co/buckets/vn-archive/mirrors/tree):
`erogamescape.sqlite3` (about 18 MB) and `vndb.sqlite3` (about 1.72 GB, November 2026).
Only the builder downloads the full archives. Downloads stream to temporary disk files;
allow several GB of free disk space. No full dump is sent to Android clients.

Updates remain **manual**, as requested:

- Actions → Game database → Run workflow: choose `all` (default), `egs`, or `vndb`.
  Both sources work on the GitHub runner. The workflow builds main and uses its package token.
- `tools/gamedb/update.sh [work dir]`: update EGS with the GitHub CLI token. Set
  `IPPAI=/path/to/ippai` to a binary built from main until a mirror-capable release exists.
- Directly: `ippai gamedb update --part egs <dir>` (or `vndb`). Authentication uses
  `GHCR_TOKEN`, otherwise `gh auth token`.

`build` and `update` accept `--mirror /path/to/downloaded.sqlite3` to avoid another download,
or `--live` to explicitly use the old websites/API. Example:

```sh
ippai gamedb build --part vndb --mirror vndb.sqlite3 gamedb-vndb.sqlite
```

EGS's mirror has game/brand data but **no POV tags or series**. The importer preserves those
fields from the previous output for surviving game IDs. `update` first pulls the published
EGS database when no local copy exists; failure stops the update. Tags/series therefore keep
their previous freshness, and newly added games have none until a `--live` update supplies
them. A first-ever `build` without a previous copy explicitly reports that it has no enrichment.

VNDB's normalized dump is converted to the existing API-shaped records: original-language
and Latin titles, official release dates/developers, ratings, cached images, aggregated
non-spoiler tags and external links. Raw user lists and individual votes are not distributed.
Source metadata and VNDB's README / ODbL / DbCL / CC0 notices are retained in `info`.

Imports check SQLite integrity, export completion, source row counts and minimum published
counts (over 20,000 EGS PC games including 30345 = LOOPERS; over 10,000 Japanese VNs).
A staged output is validated before replacing an existing file. A failed part is not published;
the workflow still attempts the other selected part. No scheduled rebuild is installed.
The individual commands remain `ippai gamedb build|pull|check|push --part`.
