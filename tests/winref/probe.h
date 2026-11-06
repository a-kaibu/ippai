/* Shared by the winref probes (tests/winref): programs whose output on real Windows is the
 * reference for sake. Everything goes to stdout, one fact per line, compared by
 * tests/winref/compare.py:
 *   plain lines                      must be equal
 *   "ulp <n> <label> <hex64>"        a double, equal within n units in the last place
 *   "image <name> <w> <h> <tol>"     then h lines of w RRGGBB hex pixels, each channel within tol
 *   "skip <reason>"                  (first line) this side cannot run the probe
 * Built with msvcrt's own printf (not mingw's), so the C runtime under test is the system's. */
#define __USE_MINGW_ANSI_STDIO 0
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <windows.h>

static void probe_ulp(int n, const char *label, double d) {
    unsigned long long u;
    memcpy(&u, &d, 8);
    printf("ulp %d %s %08lx%08lx\n", n, label, (unsigned long)(u >> 32), (unsigned long)u);
}

/* 32bpp top-down pixels (B, G, R, x) */
static void probe_image(const char *name, const void *bits, int w, int h, int stride, int tol) {
    printf("image %s %d %d %d\n", name, w, h, tol);
    for (int y = 0; y < h; y++) {
        const unsigned char *p = (const unsigned char *)bits + y * stride;
        char line[4096 * 6 + 2], *o = line;
        for (int x = 0; x < w; x++, p += 4) {
            static const char hex[] = "0123456789abcdef";
            unsigned char c[3] = { p[2], p[1], p[0] };
            for (int k = 0; k < 3; k++) { *o++ = hex[c[k] >> 4]; *o++ = hex[c[k] & 15]; }
        }
        *o = 0;
        puts(line);
    }
}

/* no error dialogs on the Windows runner */
static void __attribute__((constructor)) probe_init(void) {
    SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
    setvbuf(stdout, NULL, _IOFBF, 1 << 16);
}
