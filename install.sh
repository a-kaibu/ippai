#!/bin/sh
# Installs masu from GitHub Releases:
#
#   curl -fsSL https://raw.githubusercontent.com/a-kaibu/masu/main/install.sh | sh
#
# Puts the binary in ~/.local/bin (MASU_INSTALL_DIR to change) after checking it against the
# release's checksums.txt, and adds that directory to PATH in the shell's rc file when it is not
# on PATH yet. MASU_VERSION=v0.1.0 installs that release instead of the latest.
# The release's fonts (sake's C:\) go to ~/.local/share/masu/fonts, where `masu serve` makes
# the store's C:\ from them the first time.
set -eu

REPO=a-kaibu/masu
DIR=${MASU_INSTALL_DIR:-$HOME/.local/bin}

die() { echo "masu install: $*" >&2; exit 1; }
command -v curl >/dev/null || die "curl is needed"
command -v tar >/dev/null || die "tar is needed"

os=$(uname -s)
case "$os" in
  Linux) os=linux ;;
  *) die "$os: there are Linux builds only for now" ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=amd64 ;;
  aarch64 | arm64) arch=arm64 ;;
  *) die "$(uname -m): there are x86-64 and ARM64 builds only" ;;
esac

tag=${MASU_VERSION:-}
if [ -z "$tag" ]; then
  # (the latest release's page redirects to .../releases/tag/<tag>)
  tag=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" | sed 's#.*/tag/##')
  case "$tag" in v*) ;; *) die "no release found on https://github.com/$REPO/releases" ;; esac
fi
file="masu_${tag#v}_${os}_${arch}.tar.gz"
url="https://github.com/$REPO/releases/download/$tag"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "masu $tag ($os/$arch)"
curl -fsSL -o "$tmp/$file" "$url/$file" || die "download failed: $url/$file"
curl -fsSL -o "$tmp/checksums.txt" "$url/checksums.txt" || die "download failed: $url/checksums.txt"
want=$(grep " $file\$" "$tmp/checksums.txt" | cut -d' ' -f1)
if command -v sha256sum >/dev/null; then got=$(sha256sum "$tmp/$file" | cut -d' ' -f1)
else got=$(shasum -a 256 "$tmp/$file" | cut -d' ' -f1); fi
[ -n "$want" ] && [ "$want" = "$got" ] || die "checksum mismatch for $file"
tar -xzf "$tmp/$file" -C "$tmp"

mkdir -p "$DIR"
# (a running server keeps its old binary: replace the file instead of writing into it)
mv -f "$tmp/masu" "$DIR/masu"
chmod 755 "$DIR/masu"
echo "installed $DIR/masu"

# (what dirs.rs calls the data directory; a release from before fonts were in it has none)
if [ -d "$tmp/fonts" ]; then
  data=${XDG_DATA_HOME:-$HOME/.local/share}/masu
  mkdir -p "$data"
  rm -rf "$data/fonts.new"
  cp -R "$tmp/fonts" "$data/fonts.new"
  rm -rf "$data/fonts"
  mv "$data/fonts.new" "$data/fonts"
  echo "installed sake's fonts in $data/fonts"
fi
# (the benchmark's programs, web/bench.html: masu serve makes them its store's tree "bench")
if [ -d "$tmp/bench" ]; then
  data=${XDG_DATA_HOME:-$HOME/.local/share}/masu
  mkdir -p "$data"
  rm -rf "$data/bench.new"
  cp -R "$tmp/bench" "$data/bench.new"
  rm -rf "$data/bench"
  mv "$data/bench.new" "$data/bench"
  echo "installed the benchmark's programs in $data/bench"
fi

case ":$PATH:" in
  *":$DIR:"*) ;;
  *)
    case "$(basename "${SHELL:-sh}")" in
      zsh) rc=$HOME/.zshrc; line="export PATH=\"$DIR:\$PATH\"" ;;
      fish) rc=$HOME/.config/fish/config.fish; line="fish_add_path \"$DIR\"" ;;
      bash) rc=$HOME/.bashrc; line="export PATH=\"$DIR:\$PATH\"" ;;
      *) rc=$HOME/.profile; line="export PATH=\"$DIR:\$PATH\"" ;;
    esac
    mkdir -p "$(dirname "$rc")"
    if ! grep -qsF "$line" "$rc"; then
      printf '\n# masu\n%s\n' "$line" >> "$rc"
      echo "added $DIR to PATH in $rc (open a new shell, or: $line)"
    fi
    ;;
esac
echo "start it with: masu serve"
