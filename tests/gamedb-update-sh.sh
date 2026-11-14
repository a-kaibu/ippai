#!/bin/sh
# tools/gamedb/update.sh against a stand-in gh (sign-in, scopes, release download, token) and
# stand-in masu releases: the scope added once, the release's masu run with gh's token, an old
# release (no SQLite mirror support) and no gh refused, $MASU and a PATH masu used without a download.
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
cat > "$T/rel/new/masu" <<'EOF'
#!/bin/sh
case "$*" in "gamedb update --help") echo "      --part <PART> --mirror <PATH>" ;; --version) echo "masu 0.3.4" ;; *) echo "RUN $* token=$GHCR_TOKEN" ;; esac
EOF
cat > "$T/rel/old/masu" <<'EOF'
#!/bin/sh
case "$*" in "gamedb update --help") echo "      --part <PART>" ;; *) echo "RUN $*" ;; esac
EOF
chmod +x "$T/bin/gh" "$T/rel/new/masu" "$T/rel/old/masu"
tar -czf "$T/rel/new/masu_0.3.4_linux_amd64.tar.gz" -C "$T/rel/new" masu
tar -czf "$T/rel/old/masu_0.3.3_linux_amd64.tar.gz" -C "$T/rel/old" masu
P=$T/bin:/usr/bin:/bin
fail() { echo "FAIL: $*" >&2; exit 1; }

out=$(WHICH=new PATH=$P sh "$S" "$T/work")
echo "$out" | grep -q "RUN gamedb update --part egs $T/work token=tok123" || fail "the release's masu with gh's token: $out"
[ "$(grep -c 'auth refresh' "$T/log")" = 1 ] || fail "write:packages asked for"
grep -q "release download -R a-kaibu/masu -p masu_\*_linux_amd64.tar.gz" "$T/log" || fail "the linux release"
: > "$T/log"
WHICH=new PATH=$P sh "$S" >/dev/null
! grep -q 'auth refresh' "$T/log" || fail "the scope asked for again"
if WHICH=old PATH=$P sh "$S" 2>"$T/err"; then fail "an old release ran"; fi
grep -q "SQLite ミラー対応" "$T/err" || fail "why an old release is refused"
if MASU=$T/rel/old/masu WHICH=new PATH=$P sh "$S" 2>"$T/err"; then fail "an explicit old binary ran"; fi
grep -q "SQLite ミラー対応" "$T/err" || fail "why an explicit old binary is refused"
# (a PATH with nothing on it: the runners have a real gh in /usr/bin, which this asked to sign in
# until the step's 15 minutes ran out)
mkdir "$T/nogh"
if PATH=$T/nogh /bin/sh "$S" 2>"$T/err" </dev/null; then fail "ran without gh"; fi
grep -q "cli.github.com" "$T/err" || fail "why without gh"
: > "$T/log"
out=$(MASU=$T/rel/new/masu WHICH=old PATH=$P sh "$S" d)
echo "$out" | grep -q "RUN gamedb update --part egs d token=tok123" || fail "\$MASU: $out"
cp "$T/rel/new/masu" "$T/onpath/masu"
out=$(WHICH=old PATH=$T/onpath:$P sh "$S" d)
echo "$out" | grep -q "RUN gamedb update" || fail "masu on PATH: $out"
! grep -q "release download" "$T/log" || fail "downloaded although masu was there"
echo "gamedb update.sh: ok"
