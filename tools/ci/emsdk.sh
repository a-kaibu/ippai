# Sourced: Emscripten (the release the project builds with, 4.0.15) in $EMSDK4 (default
# ~/emsdk4), downloaded the first time. Used by tools/ci/build.sh and tests/bench/js.sh.
export EMSDK4=${EMSDK4:-$HOME/emsdk4}
EMSCRIPTEN_HASH=b412b6307e541b93dd93f01b61181e15c17302ec

if [ ! -f "$EMSDK4/emsdk_env.sh" ]; then
  mkdir -p "$EMSDK4"
  curl -fsSL "https://storage.googleapis.com/webassembly/emscripten-releases-builds/linux/$EMSCRIPTEN_HASH/wasm-binaries.tar.xz" | tar xJ -C "$EMSDK4"
  ln -sfn install "$EMSDK4/upstream"
  printf "LLVM_ROOT = '%s/upstream/bin'\nBINARYEN_ROOT = '%s/upstream'\nNODE_JS = '%s'\n" "$EMSDK4" "$EMSDK4" "$(command -v node)" > "$EMSDK4/.emscripten"
  printf 'export EMSDK=%s\nexport EM_CONFIG=%s/.emscripten\nexport PATH=%s/upstream/emscripten:%s/upstream/bin:$PATH\n' "$EMSDK4" "$EMSDK4" "$EMSDK4" "$EMSDK4" > "$EMSDK4/emsdk_env.sh"
fi
