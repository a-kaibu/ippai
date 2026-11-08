/* AVX / AVX2 256-bit forms (ymm) as the x64 JIT translates them a lane at a time: lane-wise
 * float and integer arithmetic, logic, per-lane shuffles, packs, unpacks, blends with the
 * immediate's upper bits for the upper lane, shifts by an immediate, conversions, loads and
 * stores, over many inputs (NaN, infinities, out of range for conversions, signed limits).
 * Prints checksums; they match the same program built for x86-64 Linux and run natively.
 * The functions take target("avx2") so the program needs no -mavx2. */
#include <immintrin.h>
#include <stdio.h>
#include <string.h>

#define N 64
static unsigned seed = 12345;
static unsigned rnd(void) { seed = seed * 1103515245u + 12345u; return seed ^ (seed >> 15); }

static float fin[N * 8 + 8];
static int iin[N * 8 + 8];
static unsigned long long sum;

static void mix(const void *p, int n) {
    const unsigned char *b = p;
    for (int i = 0; i < n; i++) sum = sum * 1099511628211ull ^ b[i];
}

__attribute__((target("avx2"), noinline)) static void floats(void) {
    for (int i = 0; i < N; i++) {
        __m256 a = _mm256_loadu_ps(fin + i * 8), b = _mm256_loadu_ps(fin + i * 8 + 3);
        __m256 r[13] = {
            _mm256_add_ps(a, b), _mm256_sub_ps(a, b), _mm256_mul_ps(a, b), _mm256_div_ps(a, b),
            _mm256_min_ps(a, b), _mm256_max_ps(a, b), _mm256_sqrt_ps(_mm256_andnot_ps(_mm256_set1_ps(-0.0f), a)),
            _mm256_and_ps(a, b), _mm256_xor_ps(a, b), _mm256_shuffle_ps(a, b, 0x9c),
            _mm256_blend_ps(a, b, 0x5c), _mm256_movehdup_ps(a), _mm256_blend_ps(b, a, 0xa3),
        };
        mix(r, sizeof r);
        __m256d c = _mm256_loadu_pd((double *)(fin + i * 8)), d = _mm256_loadu_pd((double *)(fin + i * 8 + 2));
        __m256d s[6] = { _mm256_add_pd(c, d), _mm256_mul_pd(c, d), _mm256_max_pd(c, d), _mm256_blend_pd(c, d, 0x9), _mm256_unpackhi_pd(c, d), _mm256_blend_pd(d, c, 0x6) };
        mix(s, sizeof s);
        __m256i t[3] = { _mm256_cvtps_epi32(a), _mm256_cvttps_epi32(b), _mm256_castps_si256(_mm256_cvtepi32_ps(_mm256_loadu_si256((__m256i *)(iin + i * 8)))) };
        mix(t, sizeof t);
    }
}

__attribute__((target("avx2"), noinline)) static void ints(void) {
    for (int i = 0; i < N; i++) {
        __m256i a = _mm256_loadu_si256((__m256i *)(iin + i * 8)), b = _mm256_loadu_si256((__m256i *)(iin + i * 8 + 5));
        __m256i r[24] = {
            _mm256_add_epi8(a, b), _mm256_adds_epi16(a, b), _mm256_subs_epu8(a, b), _mm256_add_epi64(a, b),
            _mm256_mullo_epi16(a, b), _mm256_mullo_epi32(a, b), _mm256_mulhi_epu16(a, b), _mm256_mul_epu32(a, b),
            _mm256_madd_epi16(a, b), _mm256_sad_epu8(a, b), _mm256_avg_epu8(a, b), _mm256_min_epi32(a, b),
            _mm256_max_epu16(a, b), _mm256_cmpgt_epi16(a, b), _mm256_cmpeq_epi64(a, b), _mm256_andnot_si256(a, b),
            _mm256_packs_epi32(a, b), _mm256_packus_epi16(a, b), _mm256_unpacklo_epi8(a, b), _mm256_unpackhi_epi32(a, b),
            _mm256_shuffle_epi8(a, b), _mm256_alignr_epi8(a, b, 5), _mm256_blend_epi16(a, b, 0x5a), _mm256_abs_epi8(a),
        };
        mix(r, sizeof r);
        __m256i s[7] = {
            _mm256_slli_epi16(a, 3), _mm256_srai_epi32(a, 7), _mm256_srli_epi64(a, 13), _mm256_slli_si256(a, 3),
            _mm256_srli_si256(b, 9), _mm256_shuffle_epi32(a, 0x4e), _mm256_shufflehi_epi16(b, 0x1b),
        };
        mix(s, sizeof s);
        _mm256_storeu_si256((__m256i *)(iin + i * 8 + 1), _mm256_xor_si256(a, _mm256_shufflelo_epi16(b, 0xb1)));
    }
}

int main(void) {
    unsigned fbits[] = { 0x7fc00000, 0xff800000, 0x7f800000, 0x4f000000, 0xcf000001, 0x80000000, 0x00000001, 0x3f800000 };
    for (int i = 0; i < N * 8 + 8; i++) {
        unsigned r = rnd();
        if (r % 7 == 0) memcpy(&fin[i], &fbits[r % 8], 4);
        else fin[i] = (float)((int)(rnd() % 200000) - 100000) / (float)(rnd() % 97 + 1);
        int lim[] = { 0x7fffffff, (int)0x80000000, 0, -1, 0x7fff, -0x8000, 255, 0x01010101 };
        iin[i] = rnd() % 5 == 0 ? lim[rnd() % 8] : (int)rnd();
    }
    /* (many times: hot enough for the JIT; ints() changes iin each time) */
    for (int k = 0; k < 40; k++) floats();
    printf("floats %016llx\n", sum);
    sum = 0;
    for (int k = 0; k < 40; k++) ints();
    printf("ints %016llx\n", sum);
    mix(iin, sizeof iin);
    printf("stored %016llx\n", sum);
    return 0;
}
