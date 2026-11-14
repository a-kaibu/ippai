/* The few zstd calls snapz.c makes, faked for test.c: contexts that may fail to be made. */
#include <stddef.h>
#include <string.h>
typedef struct { int x; } ZSTD_CCtx;
typedef struct { int x; } ZSTD_DCtx;
extern int fake_no_memory, fake_used_null;
static ZSTD_CCtx fake_c;
static ZSTD_DCtx fake_d;
static inline size_t ZSTD_compressBound(size_t n) { return n + 64; }
static inline ZSTD_CCtx *ZSTD_createCCtx(void) { return fake_no_memory ? NULL : &fake_c; }
static inline ZSTD_DCtx *ZSTD_createDCtx(void) { return fake_no_memory ? NULL : &fake_d; }
static inline int ZSTD_isError(size_t r) { return r > (size_t)-120; }
/* "compression": a copy (the context must be one made) */
static inline size_t ZSTD_compressCCtx(ZSTD_CCtx *c, void *d, size_t cap, const void *s, size_t n, int level) {
    (void)level;
    if (!c) { fake_used_null = 1; return (size_t)-1; }
    if (n > cap) return (size_t)-70;
    memcpy(d, s, n);
    return n;
}
static inline size_t ZSTD_decompressDCtx(ZSTD_DCtx *c, void *d, size_t cap, const void *s, size_t n) {
    if (!c) { fake_used_null = 1; return (size_t)-1; }
    if (n > cap) return (size_t)-70;
    memcpy(d, s, n);
    return n;
}
