/* MMX the way SDL 1.2's blitters use it (movq, movntq, emms, unpack/pack, lane-wise ops,
 * shifts), on x86 and x64 (the JIT translates both): one expected output. */
#include <stdio.h>
#include <string.h>
#include <mmintrin.h>
#include <xmmintrin.h>

static unsigned seed = 12345;
static unsigned rnd(void) { seed = seed * 1103515245 + 12345; return seed >> 3; }

static unsigned char src[4096] __attribute__((aligned(16))), dst[4096] __attribute__((aligned(16)));

static unsigned sum(const unsigned char *p, int n)
{
    unsigned h = 2166136261u;
    for (int i = 0; i < n; i++) h = (h ^ p[i]) * 16777619u;
    return h;
}

/* SDL_BlitCopy's MMX loop: 8 bytes at a time with non-temporal stores */
__attribute__((noinline)) static void copy_nt(unsigned char *d, const unsigned char *s, int n)
{
    for (int i = 0; i + 8 <= n; i += 8) {
        __m64 v = *(const __m64 *)(s + i);
        _mm_stream_pi((__m64 *)(d + i), v);
    }
    _mm_empty();
}

/* alpha blend of 2 pixels (ARGB8888) at a time: unpack to words, multiply, shift, pack */
__attribute__((noinline)) static void blend(unsigned char *d, const unsigned char *s, int n, int alpha)
{
    __m64 zero = _mm_setzero_si64();
    __m64 a = _mm_set1_pi16((short)alpha);
    for (int i = 0; i + 8 <= n; i += 8) {
        __m64 sv = *(const __m64 *)(s + i), dv = *(const __m64 *)(d + i);
        __m64 sl = _mm_unpacklo_pi8(sv, zero), sh = _mm_unpackhi_pi8(sv, zero);
        __m64 dl = _mm_unpacklo_pi8(dv, zero), dh = _mm_unpackhi_pi8(dv, zero);
        dl = _mm_add_pi16(dl, _mm_srli_pi16(_mm_mullo_pi16(_mm_sub_pi16(sl, dl), a), 8));
        dh = _mm_add_pi16(dh, _mm_srli_pi16(_mm_mullo_pi16(_mm_sub_pi16(sh, dh), a), 8));
        *(__m64 *)(d + i) = _mm_packs_pu16(_mm_and_si64(dl, _mm_set1_pi16(0xff)), _mm_and_si64(dh, _mm_set1_pi16(0xff)));
    }
    _mm_empty();
}

/* saturating ops, compares, shifts by register, 32-bit lanes, movd */
__attribute__((noinline)) static unsigned misc(const unsigned char *s, int n)
{
    unsigned h = 0;
    for (int i = 0; i + 16 <= n; i += 16) {
        __m64 x = *(const __m64 *)(s + i), y = *(const __m64 *)(s + i + 8);
        __m64 c = _mm_cvtsi32_si64(s[i] & 31);
        __m64 r = _mm_adds_pu8(x, y);
        r = _mm_xor_si64(r, _mm_subs_pi16(x, y));
        r = _mm_or_si64(r, _mm_cmpgt_pi8(x, y));
        r = _mm_add_pi32(r, _mm_sll_pi32(y, c));
        r = _mm_andnot_si64(_mm_sra_pi16(x, c), r);
        r = _mm_add_pi16(r, _mm_madd_pi16(x, y));
        r = _mm_xor_si64(r, _mm_packs_pi32(x, y));
        r = _mm_xor_si64(r, _mm_unpackhi_pi16(x, y));
        r = _mm_xor_si64(r, _mm_unpacklo_pi32(y, x));
        r = _mm_xor_si64(r, _mm_srli_si64(x, (i >> 4) & 63));
        h = h * 31 + (unsigned)_mm_cvtsi64_si32(r) + (unsigned)_mm_cvtsi64_si32(_mm_srli_si64(r, 32));
    }
    _mm_empty();
    return h;
}

int main(void)
{
    for (int i = 0; i < 4096; i++) src[i] = (unsigned char)rnd();
    for (int k = 0; k < 4; k++) {
        int n = 4096 - 8 * k;
        memset(dst, 0x5a, sizeof dst);
        copy_nt(dst, src + k, n);
        printf("copy %d: %08x\n", k, sum(dst, 4096));
        for (int i = 0; i < 4096; i++) dst[i] = (unsigned char)(i * 7);
        blend(dst, src, n, 37 + 50 * k);
        printf("blend %d: %08x\n", k, sum(dst, 4096));
        printf("misc %d: %08x\n", k, misc(src + k * 3, 4000));
    }
    return 0;
}
