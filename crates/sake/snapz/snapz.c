/* The page's zstd (save states, web/src/savestate.ts): Facebook's zstd, built by
 * crates/sake/build.sh into public/sake/snapz.js, run in a worker. Blocks go in and out through
 * the module's own memory (the state's memory is copied in a block at a time). */
#include <stddef.h>
#include <zstd.h>
#include <emscripten.h>

static ZSTD_CCtx *cctx;
static ZSTD_DCtx *dctx;

EMSCRIPTEN_KEEPALIVE size_t snapz_bound(size_t n) { return ZSTD_compressBound(n); }

/* `n` bytes at `src` into `dst` (`cap` bytes) at `level`: the frame's length, 0 on an error. */
EMSCRIPTEN_KEEPALIVE size_t snapz_compress(void *dst, size_t cap, const void *src, size_t n, int level)
{
    /* (no memory for a context: an error, not zstd running on NULL) */
    if (!cctx) cctx = ZSTD_createCCtx();
    if (!cctx) return 0;
    size_t r = ZSTD_compressCCtx(cctx, dst, cap, src, n, level);
    return ZSTD_isError(r) ? 0 : r;
}

/* A frame of `n` bytes into `dst`: the bytes it holds, (size_t)-1 on an error. */
EMSCRIPTEN_KEEPALIVE size_t snapz_decompress(void *dst, size_t cap, const void *src, size_t n)
{
    if (!dctx) dctx = ZSTD_createDCtx();
    if (!dctx) return (size_t)-1;
    size_t r = ZSTD_decompressDCtx(dctx, dst, cap, src, n);
    return ZSTD_isError(r) ? (size_t)-1 : r;
}
