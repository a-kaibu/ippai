# The game database

`crates/ippai/src/gamedb.rs`: ErogameScape's games and VNDB's Japanese VNs in one SQLite file,
published as ghcr.io/a-kaibu/ippai-gamedb:latest (and a tag for each day, UTC). `ippai serve`
pulls it once a day into `<store>/gamedb.sqlite`.

One command makes the day's copy: it pulls the copy published now, builds a new one (keeping the
ErogameScape part of the one before when ErogameScape does not answer), checks it and pushes it:

    GHCR_TOKEN=<token with write:packages> ippai gamedb update [--egs] <work dir>

(`--egs`: ErogameScape's part must be there. The steps also run alone: `ippai gamedb pull`,
`build [--keep]`, `check [--egs]`, `push`.)

`.github/workflows/gamedb.yml` runs it each day at 03:30 JST on GitHub's runner, out through
worker2: ErogameScape refuses GitHub's runners (and many clouds) by address and answers worker2's,
so the job joins the tailnet (tailscale/github-action, tag:ci) with worker2 as its exit node.
Without the tailnet's secrets it publishes VNDB's part and keeps ErogameScape's from before.

## Setup (once)

1. worker2 as an exit node: `sudo tailscale set --advertise-exit-node`, then in the Tailscale
   admin console -> Machines -> worker2 -> Edit route settings -> Use as exit node.
2. Access controls (tailnet policy file): a tag for the runner and its way out:

       "tagOwners": { "tag:ci": ["autogroup:admin"] },
       "acls": [ ..., { "action": "accept", "src": ["tag:ci"], "dst": ["autogroup:internet:*"] } ]

   (and, if the policy uses `grants` instead, the same as a grant). worker2's machine name is
   kota-server-worker-2; another one goes in the repository variable `GAMEDB_EXIT_NODE`.
3. Settings -> Trust credentials (OAuth clients) -> Generate: scope Auth Keys (write) with tag
   tag:ci. Its ID and secret go into the repository's secrets (github.com/a-kaibu/ippai ->
   Settings -> Secrets and variables -> Actions) as `TS_OAUTH_CLIENT_ID` and `TS_OAUTH_SECRET`.
4. Actions -> Game database -> Run workflow, once now.

The package must be public for `ippai serve` to pull it without a token: github.com/a-kaibu ->
Packages -> ippai-gamedb -> Package settings -> Change visibility -> Public.
