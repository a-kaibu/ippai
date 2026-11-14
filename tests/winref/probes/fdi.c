/* cabinet.dll's FDI as self-extracting trial games use it (#124: FDICreate, FDIIsCabinet,
 * FDICopy, FDIDestroy through the caller's own memory/file callbacks): the cabinets of
 * fdi_data.h (fdi_gen.py) read from memory; every notification with its fields, the files'
 * sizes and CRCs, what the callbacks were asked (open names and flags), the callers' answers
 * (skip, abort, a failing write), damaged and foreign cabinets and the ERF they leave.
 * libs: -lcabinet */
#include "../probe.h"
#include <fdi.h>
#include <fcntl.h>
#include "fdi_data.h"

static int live;  /* blocks from pfnalloc not yet freed */
static FNALLOC(m_alloc) { live++; return malloc(cb); }
static FNFREE(m_free) { if (pv) live--; free(pv); }

/* memory "files": 1..3 the cabinets, 10.. outputs */
struct mf { const unsigned char *p; long size, pos; unsigned char *out; long cap, len; int used; };
static struct mf files[64];
static const unsigned char *src; static long src_size; static int fail_open, fail_write_at = -1;

static FNOPEN(m_open) {
    printf("open %s flags %x mode %x\n", pszFile, oflag, pmode);
    if (fail_open) return -1;
    files[1].p = src; files[1].size = src_size; files[1].pos = 0; files[1].used = 1;
    return 1;
}
static FNREAD(m_read) {
    struct mf *f = &files[hf];
    long n = (long)cb;
    if (n > f->size - f->pos) n = f->size - f->pos;
    if (n < 0) n = 0;
    memcpy(pv, f->p + f->pos, n);
    f->pos += n;
    return n;
}
static FNWRITE(m_write) {
    struct mf *f = &files[hf];
    if (fail_write_at >= 0 && (int)hf == fail_write_at) return 0;
    if (f->len + (long)cb > f->cap) { f->cap = (f->len + cb) * 2; f->out = realloc(f->out, f->cap); }
    memcpy(f->out + f->len, pv, cb);
    f->len += cb;
    return cb;
}
static FNCLOSE(m_close) {
    if (hf == 1) printf("close cabinet\n");
    files[hf].used = 0;
    return 0;
}
static FNSEEK(m_seek) {
    struct mf *f = &files[hf];
    long base = seektype == SEEK_SET ? 0 : seektype == SEEK_CUR ? f->pos : f->size;
    f->pos = base + dist;
    return f->pos;
}

static unsigned crc32b(const unsigned char *p, long n) {
    unsigned c = 0xffffffff;
    for (long i = 0; i < n; i++) {
        c ^= p[i];
        for (int k = 0; k < 8; k++) c = c >> 1 ^ (0xedb88320 & -(c & 1));
    }
    return ~c;
}

static int next_hf, skip_mask, abort_at, close_fail, copies;
static void *expect_pv = (void *)0x1234;

static FNFDINOTIFY(notify) {
    switch (fdint) {
    case fdintCABINET_INFO:
        printf("cabinet_info next '%s' disk '%s' path '%s' set %u icab %u pv %d\n", pfdin->psz1, pfdin->psz2,
               pfdin->psz3, pfdin->setID, pfdin->iCabinet, pfdin->pv == expect_pv);
        return 0;
    case fdintCOPY_FILE: {
        int i = copies++;
        printf("copy_file '%s' cb %ld date %04x time %04x attribs %04x pv %d", pfdin->psz1, pfdin->cb, pfdin->date,
               pfdin->time, pfdin->attribs, pfdin->pv == expect_pv);
        if (i == abort_at) { printf(" -> abort\n"); return -1; }
        if (skip_mask >> i & 1) { printf(" -> skip\n"); return 0; }
        int h = next_hf++;
        free(files[h].out);
        memset(&files[h], 0, sizeof files[h]);
        files[h].used = 1;
        printf(" -> %d\n", h);
        return h;
    }
    case fdintCLOSE_FILE_INFO: {
        struct mf *f = &files[pfdin->hf];
        printf("close_file '%s' hf %d cb %ld date %04x time %04x attribs %04x len %ld crc %08x\n", pfdin->psz1, (int)pfdin->hf,
               pfdin->cb, pfdin->date, pfdin->time, pfdin->attribs, f->len, crc32b(f->out, f->len));
        f->used = 0;
        return close_fail ? FALSE : TRUE;
    }
    case fdintPARTIAL_FILE: printf("partial_file '%s'\n", pfdin->psz1); return 0;
    case fdintNEXT_CABINET: printf("next_cabinet '%s'\n", pfdin->psz1); return -1;
    case fdintENUMERATE: printf("enumerate\n"); return 0;
    }
    return 0;
}

static void run(const char *label, const unsigned char *cab, long size, int skip, int abort_, int closefail, int writefail, int openfail) {
    ERF erf;
    memset(&erf, 0x55, sizeof erf);
    src = cab; src_size = size; skip_mask = skip; abort_at = abort_; close_fail = closefail; fail_open = openfail;
    next_hf = 10; copies = 0; fail_write_at = writefail;
    printf("== %s\n", label);
    HFDI h = FDICreate(m_alloc, m_free, m_open, m_read, m_write, m_close, m_seek, cpuUNKNOWN, &erf);
    printf("create %d erf %d %d %d\n", h != NULL, erf.erfOper, erf.erfType, erf.fError);
    if (!h) return;
    files[1].p = cab; files[1].size = size; files[1].pos = 0;
    FDICABINETINFO ci;
    memset(&ci, 0x55, sizeof ci);
    /* (a file that is not a cabinet leaves the ERF alone: 天極姫's InstallShield launcher asks
     * about its .msi with the ERF of a returned function's frame) */
    memset(&erf, 0x55, sizeof erf);
    BOOL is = FDIIsCabinet(h, 1, &ci);
    printf("iscabinet %d", is);
    if (is) printf(" cb %ld folders %u files %u set %u icab %u reserve %d prev %d next %d", ci.cbCabinet, ci.cFolders, ci.cFiles,
                   ci.setID, ci.iCabinet, ci.fReserve, ci.hasprev, ci.hasnext);
    else printf(" erf %d %d %d", erf.erfOper, erf.erfType, erf.fError);
    printf("\n");
    memset(&erf, 0x55, sizeof erf);
    char name[] = "trial.cab", path[] = "C:\\probe\\";
    BOOL r = FDICopy(h, name, path, 0, notify, NULL, expect_pv);
    printf("copy %d", r);
    if (!r) printf(" erf %d %d", erf.erfOper, erf.fError);
    printf("\n");
    BOOL d = FDIDestroy(h);
    printf("destroy %d live %d\n", d, live);
}

int main(void) {
    run("main", cab_main, sizeof cab_main, 0, -1, 0, -1, 0);
    run("skip odd", cab_main, sizeof cab_main, 0x2a, -1, 0, -1, 0);
    run("abort third", cab_main, sizeof cab_main, 0, 2, 0, -1, 0);
    run("close fails", cab_main, sizeof cab_main, 0, -1, 1, -1, 0);
    run("write fails", cab_main, sizeof cab_main, 0, -1, 0, 12, 0);
    run("open fails", cab_main, sizeof cab_main, 0, -1, 0, -1, 1);
    run("reserved", cab_reserved, sizeof cab_reserved, 0, -1, 0, -1, 0);
    run("version 1.4", cab_newer, sizeof cab_newer, 0, -1, 0, -1, 0);
    run("truncated data", cab_main, sizeof cab_main - 40, 0, -1, 0, -1, 0);
    static const unsigned char junk[64] = "MZ this is not a cabinet at all";
    run("not a cabinet", junk, sizeof junk, 0, -1, 0, -1, 0);
    return 0;
}
