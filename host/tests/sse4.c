/* SSE4.1/4.2 instructions the interpreter runs from their own code (dpps, dppd, mpsadbw,
 * phminposuw, pcmpestri/m, pcmpistri/m): checksums over pseudo-random inputs and every
 * immediate that matters. The expected output comes from the same file built natively on
 * x86-64 Linux (gcc -O2 -msse4.2 sse4.c).
 * libs: -msse4.2 -mfpmath=sse */
#include <nmmintrin.h>
#include <smmintrin.h>
#include <stdio.h>
#include <string.h>

static unsigned seed = 12345;
static unsigned rnd(void)
{
    seed = seed * 1103515245 + 12345;
    return seed >> 8;
}

static unsigned h = 2166136261u;
static void mix(const void *p, int n)
{
    const unsigned char *b = p;
    while (n--) h = (h ^ *b++) * 16777619u;
}
static void mixv(__m128i v)
{
    unsigned char b[16];
    memcpy(b, &v, 16);
    mix(b, 16);
}

/* a vector of small letters (so that compares hit), sometimes with a zero in it */
static __m128i text(void)
{
    unsigned char b[16];
    for (int i = 0; i < 16; i++) b[i] = (unsigned char)("abcdeAZ09 \x80\xff"[rnd() % 13]);
    if (rnd() % 3 == 0) b[rnd() % 16] = 0;
    __m128i v;
    memcpy(&v, b, 16);
    return v;
}

#define STR(IMM)                                                                \
    do {                                                                        \
        int la = (int)(rnd() % 40) - 20, lb = (int)(rnd() % 40) - 20;           \
        int r[6];                                                               \
        r[0] = _mm_cmpistri(a, b, IMM);                                         \
        r[1] = _mm_cmpestri(a, la, b, lb, IMM);                                 \
        r[2] = _mm_cmpistrc(a, b, IMM) | _mm_cmpistrz(a, b, IMM) << 1 |         \
               _mm_cmpistrs(a, b, IMM) << 2 | _mm_cmpistro(a, b, IMM) << 3;     \
        r[3] = _mm_cmpestrc(a, la, b, lb, IMM) | _mm_cmpestrz(a, la, b, lb, IMM) << 1 | \
               _mm_cmpestrs(a, la, b, lb, IMM) << 2 | _mm_cmpestro(a, la, b, lb, IMM) << 3; \
        r[4] = r[5] = 0;                                                        \
        mix(r, sizeof r);                                                       \
        mixv(_mm_cmpistrm(a, b, IMM));                                          \
        mixv(_mm_cmpestrm(a, la, b, lb, IMM));                                  \
    } while (0)

#define STR4(F) STR(F | 0x00); STR(F | 0x10); STR(F | 0x30); STR(F | 0x40); STR(F | 0x70)

int main(void)
{
    /* dot products */
    for (int k = 0; k < 200; k++) {
        float x[8];
        for (int i = 0; i < 8; i++) x[i] = (float)((int)(rnd() % 2001) - 1000) / 37.0f;
        __m128 a = _mm_loadu_ps(x), b = _mm_loadu_ps(x + 4);
        __m128d c = _mm_loadu_pd((double[]){x[0], x[1]}), d = _mm_loadu_pd((double[]){x[2], x[3]});
        __m128 r[4] = {_mm_dp_ps(a, b, 0xff), _mm_dp_ps(a, b, 0x71), _mm_dp_ps(a, b, 0xa6), _mm_dp_ps(a, b, 0x3c)};
        __m128d s[3] = {_mm_dp_pd(c, d, 0x33), _mm_dp_pd(c, d, 0x12), _mm_dp_pd(c, d, 0x21)};
        mix(r, sizeof r);
        mix(s, sizeof s);
    }
    printf("dp          %08x\n", h);

    h = 2166136261u;
    for (int k = 0; k < 200; k++) {
        unsigned char x[32];
        for (int i = 0; i < 32; i++) x[i] = (unsigned char)rnd();
        __m128i a = _mm_loadu_si128((void *)x), b = _mm_loadu_si128((void *)(x + 16));
        mixv(_mm_mpsadbw_epu8(a, b, 0));
        mixv(_mm_mpsadbw_epu8(a, b, 1));
        mixv(_mm_mpsadbw_epu8(a, b, 2));
        mixv(_mm_mpsadbw_epu8(a, b, 3));
        mixv(_mm_mpsadbw_epu8(a, b, 4));
        mixv(_mm_mpsadbw_epu8(a, b, 7));
        mixv(_mm_minpos_epu16(a));
        if (k % 10 == 0) mixv(_mm_minpos_epu16(_mm_set1_epi16((short)rnd())));
    }
    printf("mpsadbw     %08x\n", h);

    h = 2166136261u;
    for (int k = 0; k < 300; k++) {
        __m128i a = text(), b = text();
        /* formats: unsigned/signed bytes and words; aggregations: any, ranges, each, ordered */
        STR4(0x00); STR4(0x01); STR4(0x02); STR4(0x03);
        STR4(0x04); STR4(0x05); STR4(0x06); STR4(0x07);
        STR4(0x08); STR4(0x09); STR4(0x0a); STR4(0x0b);
        STR4(0x0c); STR4(0x0d); STR4(0x0e); STR4(0x0f);
    }
    printf("pcmpstr     %08x\n", h);
    return 0;
}
