#!/usr/bin/env python3
"""The game database (crates/ippai/src/gamedb.rs, built by `ippai gamedb build`) onto GitHub
Packages: ghcr.io/a-kaibu/ippai-gamedb, tagged latest and the build's date (UTC), zstd-compressed,
an OCI artifact linked to the repository (a public package: ippai serve pulls it anonymously).
Needs GHCR_TOKEN (CI: the job's GITHUB_TOKEN with packages: write); GAMEDB_REF for another
registry.

    push.py <gamedb.sqlite>
"""
import datetime
import os
import subprocess
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "tests", "replay"))
from games import EMPTY, SOURCE, Registry  # noqa: E402

REF = os.environ.get("GAMEDB_REF", "ghcr.io/a-kaibu/ippai-gamedb")


def main():
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    data = open(sys.argv[1], "rb").read()
    z = subprocess.run(["zstd", "-19", "-T0", "-q", "-c"], input=data, stdout=subprocess.PIPE, check=True).stdout
    reg = Registry(REF)
    layer = {"mediaType": "application/zstd", "digest": reg.put_blob(z), "size": len(z),
             "annotations": {"org.opencontainers.image.title": "gamedb.sqlite.zst"}}
    m = {"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
         "artifactType": "application/vnd.ippai.gamedb.v1",
         "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": reg.put_blob(EMPTY), "size": len(EMPTY)},
         "layers": [layer],
         "annotations": {"org.opencontainers.image.source": SOURCE,
                         "org.opencontainers.image.description": "ErogameScape and VNDB entries for ippai (crates/ippai/src/gamedb.rs)"}}
    for tag in ("latest", datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%d")):
        reg.put_manifest(tag, m)
    print(f"{REF}:latest {layer['digest']} ({len(data)} bytes, {len(z)} compressed)")


if __name__ == "__main__":
    main()
