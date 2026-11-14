/* AVX / AVX2 256-bit forms (ymm) as the x64 JIT translates them a lane at a time: lane-wise
 * float and integer arithmetic, logic, per-lane shuffles, packs, unpacks, blends with the
 * immediate's upper bits for the upper lane, shifts by an immediate, conversions, loads and
 * stores, over many inputs (NaN, infinities, out of range for conversions, signed limits); and
 * the forms that cross lanes or have no legacy form (cross()).
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

/* forms that move data between the lanes or have no legacy form (jit/long/avx.rs): inserts,
 * extracts, VPERM2F128 (zeroing bits too), VPERMQ/PD, VPERMD/PS (indices above 7), broadcasts
 * from registers and memory, VPERMILPS/PD (both forms), VPBLENDD, the variable shifts with
 * counts around the element width, VPMOVZX/SX into YMM, VPMOVMSKB/VMOVMSKPS/PD of YMM */
__attribute__((target("avx2"), noinline)) static void cross(void) {
    for (int i = 0; i < N; i++) {
        __m256i a = _mm256_loadu_si256((__m256i *)(iin + i * 8)), b = _mm256_loadu_si256((__m256i *)(iin + i * 8 + 3));
        __m256i n = _mm256_and_si256(b, _mm256_set1_epi32(0x47)), n64 = _mm256_and_si256(b, _mm256_set1_epi64x(0x47));
        __m256 f = _mm256_castsi256_ps(a), g = _mm256_castsi256_ps(b);
        __m256d d = _mm256_castsi256_pd(a), e = _mm256_castsi256_pd(b);
        __m128i x = _mm_loadu_si128((__m128i *)(iin + i * 8 + 2));
        __m256i r[34] = {
            _mm256_inserti128_si256(a, x, 0), _mm256_inserti128_si256(a, x, 1),
            _mm256_zextsi128_si256(_mm256_extracti128_si256(a, 1)), _mm256_zextsi128_si256(_mm256_extractf128_si256(b, 0)),
            _mm256_permute2x128_si256(a, b, 0x21), _mm256_permute2x128_si256(a, b, 0x83), _mm256_permute2x128_si256(a, b, 0x38),
            _mm256_permute4x64_epi64(a, 0x1b), _mm256_permute4x64_epi64(b, 0xe4),
            _mm256_castpd_si256(_mm256_permute4x64_pd(d, 0x72)),
            _mm256_permutevar8x32_epi32(a, b), _mm256_castps_si256(_mm256_permutevar8x32_ps(g, a)),
            _mm256_broadcastd_epi32(x), _mm256_broadcastb_epi8(x), _mm256_broadcastw_epi16(_mm256_castsi256_si128(b)),
            _mm256_broadcastq_epi64(x), _mm256_castps_si256(_mm256_broadcastss_ps(_mm_castsi128_ps(x))),
            _mm256_castps_si256(_mm256_broadcast_ss(fin + i)), _mm256_castpd_si256(_mm256_broadcast_sd((double *)(fin + i))),
            _mm256_broadcastsi128_si256(x), _mm256_castps_si256(_mm256_permute_ps(f, 0x39)),
            _mm256_castpd_si256(_mm256_permute_pd(d, 0x6)), _mm256_castps_si256(_mm256_permutevar_ps(f, b)),
            _mm256_castpd_si256(_mm256_permutevar_pd(d, b)), _mm256_blend_epi32(a, b, 0xa5),
            _mm256_sllv_epi32(a, n), _mm256_srlv_epi32(a, n), _mm256_srav_epi32(a, n),
            _mm256_sllv_epi64(a, n64), _mm256_srlv_epi64(a, n64),
            _mm256_cvtepu8_epi16(x), _mm256_cvtepi8_epi32(x), _mm256_cvtepu16_epi64(x), _mm256_cvtepi32_epi64(x),
        };
        mix(r, sizeof r);
        __m128i h[8] = {
            _mm_sllv_epi32(x, _mm256_castsi256_si128(n)), _mm_srav_epi32(x, _mm256_castsi256_si128(n)), _mm_srlv_epi64(x, _mm256_castsi256_si128(n64)),
            _mm_blend_epi32(x, _mm256_castsi256_si128(b), 0x6), _mm_castps_si128(_mm_permutevar_ps(_mm_castsi128_ps(x), _mm256_castsi256_si128(b))),
            _mm_castpd_si128(_mm_permute_pd(_mm_castsi128_pd(x), 1)), _mm_broadcastb_epi8(x), _mm_castps_si128(_mm_broadcastss_ps(_mm_castsi128_ps(x))),
        };
        mix(h, sizeof h);
        int m[3] = { _mm256_movemask_epi8(a), _mm256_movemask_ps(g), _mm256_movemask_pd(e) };
        mix(m, sizeof m);
        __m128i lo = _mm256_extracti128_si256(b, 1);
        _mm_storeu_si128((__m128i *)(iin + i * 8 + 6), _mm_xor_si128(lo, _mm256_castsi256_si128(a)));
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
    sum = 0;
    for (int k = 0; k < 40; k++) cross();
    printf("cross %016llx\n", sum);
    mix(iin, sizeof iin);
    printf("cross stored %016llx\n", sum);
    return 0;
}
