#!/bin/sh
# Builds Berkeley TestFloat 3e's testfloat_gen (with SoftFloat 3e, John Hauser's IEEE 754
# reference, specialized for x86 SSE: 8086-SSE NaN rules) to target/testfloat. Pinned commits,
# fetched from GitHub; nothing of them is committed here (BSD licensed, kept outside the repo).
# crates/koji/tests/testfloat.rs and tools/jit32test (`testfloat`) run it.
set -eu
SF=07ebd29db0cfb55723a79fbbffe99103ef38e305
TF=a9c849f1b0eb0264b626d9686ffae167d996e3be
cd "$(dirname "$0")/../.."
OUT=target/testfloat
STAMP="$SF $TF"
if [ -x "$OUT/testfloat_gen" ] && [ "$(cat "$OUT/stamp" 2>/dev/null)" = "$STAMP" ]; then
  exit 0
fi
rm -rf "$OUT"
mkdir -p "$OUT"
fetch() { # repo rev dir
  git init -q "$OUT/$3"
  git -C "$OUT/$3" -c gc.auto=0 fetch -q --depth 1 "https://github.com/ucb-bar/$1" "$2"
  git -C "$OUT/$3" -c gc.auto=0 checkout -q FETCH_HEAD
}
fetch berkeley-softfloat-3 "$SF" softfloat
fetch berkeley-testfloat-3 "$TF" testfloat
J=$(nproc 2>/dev/null || echo 2)
make -s -C "$OUT/softfloat/build/Linux-x86_64-GCC" -j"$J" SPECIALIZE_TYPE=8086-SSE softfloat.a
make -s -C "$OUT/testfloat/build/Linux-x86_64-GCC" -j"$J" SPECIALIZE_TYPE=8086-SSE SOFTFLOAT_DIR=../../../softfloat testfloat_gen
cp "$OUT/testfloat/build/Linux-x86_64-GCC/testfloat_gen" "$OUT/testfloat_gen"
echo "$STAMP" > "$OUT/stamp"
echo "testfloat_gen: $OUT/testfloat_gen"
