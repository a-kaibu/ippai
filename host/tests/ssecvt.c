/* SSE2 conversions and compares the JIT translates (CVTTSD2SI, CVTSS2SD, COMISS, MOVSD, BT...):
   edge values (NaN, out of range, -0, halves) folded into a checksum; the same with
   IPPAI_JIT=0 (and natively). Build with -msse2 -mssse3 (PSHUFB). */
#include <stdio.h>
#include <string.h>
#include <tmmintrin.h>

static unsigned h = 2166136261u;
static void mix(unsigned v) { h = (h ^ v) * 16777619u; }
static void mixd(double d) { unsigned long long u; memcpy(&u, &d, 8); mix((unsigned)u); mix((unsigned)(u >> 32)); }
static void mixf(float f) { unsigned u; memcpy(&u, &f, 4); mix(u); }

static volatile double dv[] = {0.0, -0.0, 1.5, -1.5, 2.5, -2.9, 2147483647.0, 2147483647.9, 2147483648.0, -2147483648.0,
    -2147483648.9, -2147483649.0, 1e300, -1e300, 3.4e38, 1e-310, __builtin_nan(""), __builtin_inf(), -__builtin_inf(), 0.1};
static volatile int iv[] = {0, 1, -1, 0x7fffffff, (int)0x80000000, 16777217, -16777217, 123456789};

int main(void) {
    int n = sizeof dv / sizeof dv[0];
    for (int rep = 0; rep < 2000; rep++) {
        for (int i = 0; i < n; i++) {
            double d = dv[i];
            float f = (float)d;
            mixf(f);
            mixd((double)f);
            mix((unsigned)_mm_cvttsd_si32(_mm_set_sd(d)));
            mix((unsigned)_mm_cvttss_si32(_mm_set_ss(f)));
            for (int j = 0; j < n; j++) {
                double e = dv[j];
                __m128d a = _mm_set_sd(d), b = _mm_set_sd(e);
                mix(_mm_comilt_sd(a, b) | _mm_comieq_sd(a, b) << 1 | _mm_ucomigt_sd(a, b) << 2 | _mm_ucomile_sd(a, b) << 3 | _mm_comineq_sd(a, b) << 4);
                __m128 x = _mm_set_ss((float)d), y = _mm_set_ss((float)e);
                mix(_mm_comilt_ss(x, y) | _mm_comieq_ss(x, y) << 1 | _mm_ucomigt_ss(x, y) << 2 | _mm_ucomige_ss(x, y) << 3 | _mm_ucomineq_ss(x, y) << 4);
                /* branches on the flags (jp/jb/ja after ucomisd) */
                if (d < e) mix(1); else if (d == e) mix(2); else if (d > e) mix(3); else mix(4);
                if ((float)d <= (float)e) mix(5);
            }
            __m128d p = _mm_cvtps_pd(_mm_set_ps(1, 2, f, -f));
            mixd(_mm_cvtsd_f64(p)); mixd(_mm_cvtsd_f64(_mm_unpackhi_pd(p, p)));
            __m128 q = _mm_cvtpd_ps(_mm_set_pd(d, -d));
            mixf(_mm_cvtss_f32(q)); mixf(_mm_cvtss_f32(_mm_shuffle_ps(q, q, 1))); mixf(_mm_cvtss_f32(_mm_shuffle_ps(q, q, 2)));
            __m128d s = _mm_cvtss_sd(_mm_set_pd(7.0, 9.0), _mm_set_ss(f));
            mixd(_mm_cvtsd_f64(s)); mixd(_mm_cvtsd_f64(_mm_unpackhi_pd(s, s)));
            __m128 t = _mm_cvtsd_ss(_mm_set_ps(5, 6, 7, 8), _mm_set_sd(d));
            mixf(_mm_cvtss_f32(t)); mixf(_mm_cvtss_f32(_mm_shuffle_ps(t, t, 3)));
            __m128d m = _mm_move_sd(_mm_set_pd(3.0, 4.0), _mm_set_sd(d));
            mixd(_mm_cvtsd_f64(_mm_unpackhi_pd(m, m))); mixd(_mm_cvtsd_f64(m));
        }
        for (int i = 0; i < (int)(sizeof iv / sizeof iv[0]); i++) {
            int v = iv[i];
            mixd(_mm_cvtsd_f64(_mm_cvtsi32_sd(_mm_set_pd(1.0, 2.0), v)));
            mixf(_mm_cvtss_f32(_mm_cvtsi32_ss(_mm_set1_ps(3.0f), v)));
            __m128d c = _mm_cvtepi32_pd(_mm_set_epi32(9, 8, v, -v));
            mixd(_mm_cvtsd_f64(c)); mixd(_mm_cvtsd_f64(_mm_unpackhi_pd(c, c)));
            __m128 c2 = _mm_cvtepi32_ps(_mm_set_epi32(v, 8, -v, v ^ 0x5555));
            mixf(_mm_cvtss_f32(c2)); mixf(_mm_cvtss_f32(_mm_shuffle_ps(c2, c2, 3)));
            for (int k = 0; k < 40; k += 3) {
                int bit;
                __asm__ volatile("bt %2, %1; setc %b0; movzbl %b0, %0" : "=q"(bit) : "r"(v), "r"(k) : "cc");
                mix(bit);
                __asm__ volatile("xor %0, %0; bt $5, %1; jnc 1f; inc %0; 1:" : "=&r"(bit) : "r"(v ^ k) : "cc");
                mix(bit);
                /* ZF kept by BT on hardware: cmp sets it, bt, then setz */
                __asm__ volatile("cmp %1, %2; bt %2, %1; setz %b0; movzbl %b0, %0" : "=&q"(bit) : "r"(v), "r"(k) : "cc");
                mix(bit);
            }
        }
        /* PSHUFB (SSSE3): index bytes with the high bit set give 0 */
        for (int k = 0; k < 16; k++) {
            unsigned seed = (unsigned)(rep * 16 + k) * 2654435761u;
            __m128i a = _mm_set_epi32((int)seed, (int)(seed * 3), (int)(seed ^ 0x5a5a5a5a), (int)(seed * 7));
            __m128i b = _mm_set_epi32((int)(seed * 11), (int)(seed * 13 + 0x80808080u), (int)(seed >> 3), (int)(seed * 17));
            __m128i r = _mm_shuffle_epi8(a, b);
            mix(_mm_cvtsi128_si32(r)); mix(_mm_extract_epi16(r, 3)); mix(_mm_extract_epi16(r, 7)); mix(_mm_extract_epi16(r, 5));
            unsigned long long ma = (unsigned long long)seed * 0x9e3779b97f4a7c15ull, mb = ma * 31 ^ 0x8000800080008000ull;
            __asm__ volatile("movq %1, %%mm0; movq %2, %%mm1; pshufb %%mm1, %%mm0; movq %%mm0, %0; emms" : "=m"(ma) : "m"(ma), "m"(mb) : "mm0", "mm1");
            mix((unsigned)ma); mix((unsigned)(ma >> 32));
        }
        unsigned mx;
        __asm__ volatile("stmxcsr %0" : "=m"(mx));
        mix(mx);
    }
    printf("ssecvt %08x\n", h);
    return 0;
}
