/* Delphi's System.Move (QLIE かみぱに！ 0x402db0, register call: eax=src, edx=dst, ecx=n) on random
 * overlapping copies, checked against memmove. Build: i686-w64-mingw32-gcc -O2 delphimove.c */
#include <stdio.h>
#include <string.h>

__asm__(".globl _dmove\n_dmove:\n"
        ".byte 0x56,0x57,0x89,0xc6,0x89,0xd7,0x89,0xc8,0x39,0xf7,0x77,0x13,0x74,0x2f\n"
        ".byte 0xc1,0xf9,0x02,0x78,0x2a,0xf3,0xa5,0x89,0xc1,0x83,0xe1,0x03,0xf3,0xa4\n"
        ".byte 0x5f,0x5e,0xc3\n"
        ".byte 0x8d,0x74,0x31,0xfc,0x8d,0x7c,0x39,0xfc,0xc1,0xf9,0x02,0x78,0x11,0xfd\n"
        ".byte 0xf3,0xa5,0x89,0xc1,0x83,0xe1,0x03,0x83,0xc6,0x03,0x83,0xc7,0x03,0xf3,0xa4,0xfc\n"
        ".byte 0x5f,0x5e,0xc3\n");

static void move(const void *s, void *d, int n)
{
    int a, b, c;
    __asm__ volatile("call _dmove" : "=a"(a), "=d"(b), "=c"(c) : "a"(s), "d"(d), "c"(n) : "memory", "cc");
}

static unsigned seed = 1;
static unsigned rnd(void) { return seed = seed * 1103515245 + 12345, seed >> 8; }

int main(int argc, char **argv)
{
    static unsigned char a[8192], b[8192];
    unsigned i, k, bad = 0, df, nils = argc < 2;
    for (i = 0; i < sizeof a; i++) a[i] = b[i] = (unsigned char)rnd();
    for (k = 0; k < 400000; k++) {
        int n = rnd() % 300, s = rnd() % 4000, d = rnd() % 4000;
        if (k % 7 == 0) d = s + (int)(rnd() % 4) + 1 - (k % 2) * 5; /* close overlaps, never d == s (a region without the je target) */
        if (d < 0) d = 0;
        move(a + s, a + d, n);
        memmove(b + d, b + s, n);
        if (nils && k % 13 == 0) move(NULL, a + d, 0), move(a + s, NULL, 0), move((const char *)main + 1, NULL, 0);
        if (k == 1000 || k == 300000) move((const char *)main + 1, NULL, 0);
        __asm__ volatile("pushfl; popl %0" : "=r"(df));
        if (df & 0x400) { printf("DF left set at %u\n", k); return 1; }
        if (memcmp(a, b, sizeof a)) {
            if (bad++ < 5) printf("mismatch at %u: s=%d d=%d n=%d\n", k, s, d, n);
            memcpy(a, b, sizeof a);
        }
    }
    printf("delphimove %s (%u bad)\n", bad ? "FAIL" : "ok", bad);
    return bad != 0;
}
