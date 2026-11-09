#!/bin/sh
# tools/gamedb/update.sh against a stand-in gh (sign-in, scopes, release download, token) and
# stand-in ippai releases: the scope added once, the release's ippai run with gh's token, an old
# release (no `gamedb update`) and no gh refused, $IPPAI and a PATH ippai used without a download.
set -eu
S=$(cd "$(dirname "$0")/.." && pwd)/tools/gamedb/update.sh
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
export T
mkdir -p "$T/bin" "$T/onpath" "$T/rel/new" "$T/rel/old"
cat > "$T/bin/gh" <<'EOF'
#!/bin/sh
echo "gh $*" >> "$T/log"
case "$1 $2" in
  "auth status") if [ -f "$T/scoped" ]; then echo "  - Token scopes: 'repo', 'write:packages'"; else echo "  - Token scopes: 'repo'"; fi ;;
  "auth refresh") touch "$T/scoped" ;;
  "auth token") echo tok123 ;;
  "release download") cp "$T/rel/$WHICH"/*.tar.gz "$8" ;;
esac
EOF
cat > "$T/rel/new/ippai" <<'EOF'
#!/bin/sh
case "$*" in "gamedb update --help") echo "      --part <PART>" ;; --version) echo "ippai 0.3.4" ;; *) echo "RUN $* token=$GHCR_TOKEN" ;; esac
EOF
cat > "$T/rel/old/ippai" <<'EOF'
#!/bin/sh
case "$*" in "gamedb update --help") echo "      --egs" ;; *) echo "RUN $*" ;; esac
EOF
chmod +x "$T/bin/gh" "$T/rel/new/ippai" "$T/rel/old/ippai"
tar -czf "$T/rel/new/ippai_0.3.4_linux_amd64.tar.gz" -C "$T/rel/new" ippai
tar -czf "$T/rel/old/ippai_0.3.3_linux_amd64.tar.gz" -C "$T/rel/old" ippai
P=$T/bin:/usr/bin:/bin
fail() { echo "FAIL: $*" >&2; exit 1; }

out=$(WHICH=new PATH=$P sh "$S" "$T/work")
echo "$out" | grep -q "RUN gamedb update --part egs $T/work token=tok123" || fail "the release's ippai with gh's token: $out"
[ "$(grep -c 'auth refresh' "$T/log")" = 1 ] || fail "write:packages asked for"
grep -q "release download -R a-kaibu/ippai -p ippai_\*_linux_amd64.tar.gz" "$T/log" || fail "the linux release"
: > "$T/log"
WHICH=new PATH=$P sh "$S" >/dev/null
! grep -q 'auth refresh' "$T/log" || fail "the scope asked for again"
if WHICH=old PATH=$P sh "$S" 2>"$T/err"; then fail "an old release ran"; fi
grep -q "v0.3.4" "$T/err" || fail "why an old release is refused"
if PATH=/usr/bin:/bin sh "$S" 2>"$T/err"; then fail "ran without gh"; fi
grep -q "cli.github.com" "$T/err" || fail "why without gh"
: > "$T/log"
out=$(IPPAI=$T/rel/new/ippai WHICH=old PATH=$P sh "$S" d)
echo "$out" | grep -q "RUN gamedb update --part egs d token=tok123" || fail "\$IPPAI: $out"
cp "$T/rel/new/ippai" "$T/onpath/ippai"
out=$(WHICH=old PATH=$T/onpath:$P sh "$S" d)
echo "$out" | grep -q "RUN gamedb update" || fail "ippai on PATH: $out"
! grep -q "release download" "$T/log" || fail "downloaded although ippai was there"
echo "gamedb update.sh: ok"
