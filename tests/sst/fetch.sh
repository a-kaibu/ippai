#!/bin/sh
# Downloads SingleStepTests/80386 (real-mode tests recorded on a 386EX, ~430 MB) to target/sst
# for crates/koji/tests/singlestep.rs. Pinned to one commit; nothing of it is committed here.
set -eu
REV=459d49fbe6280e9ed46fee887b58dacd9cb880ab
cd "$(dirname "$0")/../.."
OUT=target/sst
if [ "$(git -C "$OUT" rev-parse HEAD 2>/dev/null || true)" = "$REV" ] && [ -d "$OUT/v1_ex_real_mode" ]; then
  exit 0
fi
rm -rf "$OUT"
git init -q "$OUT"
git -C "$OUT" -c gc.auto=0 fetch -q --depth 1 --filter=blob:none https://github.com/SingleStepTests/80386 "$REV"
git -C "$OUT" -c gc.auto=0 checkout -q FETCH_HEAD -- v1_ex_real_mode revocation_list.txt
git -C "$OUT" -c gc.auto=0 update-ref HEAD FETCH_HEAD
echo "SingleStepTests/80386 $REV: $(ls "$OUT/v1_ex_real_mode" | wc -l) files in $OUT"
