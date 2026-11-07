/* The MXCSR exception flags as programs read them. A hot SSE loop first, before anything read
 * the flags (sake's JIT keeps them only from the program's first read: koji::jit::MX_EXACT;
 * what that loop raised is lost, so _clearfp's answer there isn't printed), then the flags
 * cleared and loops whose flags are read, in a loop of its own: the values all along, and the
 * flags from the clear on, as the CPU's. _statusfp, _clearfp and STMXCSR agree. Then an x87
 * FRNDINT's flags (what each of them answers), and MXCSR's FTZ and DAZ (0x9fc0): denormal
 * operands and results read and come out as zero, with the flags those raise. */
#include "../probe.h"
#include <float.h>
#include <emmintrin.h>

/* _SW_INEXACT, _SW_UNDERFLOW, _SW_OVERFLOW, _SW_ZERODIVIDE, _SW_INVALID */
#define SW 0x1f

static unsigned getcsr(void) {
    unsigned m;
    __asm__ volatile("stmxcsr %0" : "=m"(m));
    return m;
}
static void setcsr(unsigned m) { __asm__ volatile("ldmxcsr %0" : : "m"(m)); }

static volatile float in[] = { 1.0f, 3.0f, 0.0f, 1e-30f, 3e38f, -1.0f, 0.5f };

/* scalar and packed SSE floats: k picks what they raise (none, inexact, underflow, divide by
 * zero, invalid, overflow, denormal results) */
static unsigned work(int n, int k) {
    __m128 s = _mm_set1_ps(in[0]);
    unsigned h = 0;
    for (int i = 0; i < n; i++) {
        __m128 x = _mm_set_ss((float)(i % 7 + 1));
        __m128 a = _mm_set_ss(in[k == 0 ? 0 : k == 2 ? 3 : k == 5 ? 4 : 1]);
        __m128 b = _mm_set_ss(in[k == 0 ? 0 : k == 3 || k == 4 ? 2 : k == 2 ? 3 : k == 5 ? 4 : 1]);
        __m128 r;
        switch (k) {
        case 0: r = _mm_add_ss(a, b); break;                   /* exact */
        case 1: r = _mm_div_ss(x, b); break;                   /* inexact */
        case 2: r = _mm_mul_ss(a, b); break;                   /* underflow */
        case 3: r = _mm_div_ss(x, b); break;                   /* divide by zero */
        case 4: r = _mm_div_ss(b, b); break;                   /* invalid */
        case 5: r = _mm_mul_ss(a, b); break;                   /* overflow */
        default: r = _mm_mul_ps(_mm_set1_ps(1e-20f), _mm_set1_ps(1e-19f)); /* denormal */
        }
        /* (the sum keeps denormals: the exponent bits masked) */
        s = _mm_add_ps(s, _mm_and_ps(r, _mm_castsi128_ps(_mm_set1_epi32(0x80ffffff))));
        h = h * 31 + (unsigned)_mm_cvtsi128_si32(_mm_castps_si128(r));
    }
    float f[4];
    _mm_storeu_ps(f, s);
    for (int j = 0; j < 4; j++) {
        unsigned u;
        memcpy(&u, &f[j], 4);
        h = h * 31 + u;
    }
    return h;
}

int main(void) {
    setcsr(0x1f80);
    for (int k = 0; k < 7; k++)
        printf("before any read: k %d value %08x\n", k, work(200000, k));
    _clearfp();
    printf("cleared: mxcsr %#x status %#x\n", getcsr(), _statusfp());
    for (int round = 0; round < 3; round++)
        for (int k = 0; k < 7; k++) {
            setcsr(getcsr() & ~0x3f);
            unsigned v = work(50000, k);
            unsigned m = getcsr() & 0x3f;
            unsigned st = _statusfp() & SW;
            unsigned cl = _clearfp() & SW;
            printf("round %d k %d value %08x mxcsr flags %#x _statusfp %#x _clearfp %#x after %#x\n", round, k, v, m, st, cl, getcsr() & 0x3f);
        }
    {
        volatile double half = 2.5, q;
        unsigned short sw;
        _clearfp();
        setcsr(0x1f80);
        __asm__ volatile("fldl %1\n\tfrndint\n\tfstpl %0" : "=m"(q) : "m"(half));
        __asm__ volatile("fnstsw %0" : "=m"(sw));
        unsigned mx = getcsr() & 0x3f;
        unsigned st = _statusfp() & SW, cl = _clearfp() & SW;
        (void)q;
        printf("frndint 2.5: x87 flags %#x mxcsr flags %#x -> _statusfp %#x _clearfp %#x after %#x %#x\n", sw & 0x3f, mx, st, cl,
               _statusfp() & SW, getcsr() & 0x3f);
    }
    static volatile unsigned den[] = { 1, 0x80000001, 0x007fffff, 0x00800000, 0x3f800000, 0x0d000000, 0x33000000 };
    for (int i = 0; i < 7; i++)
        for (int j = 0; j < 7; j++) {
            setcsr(0x9fc0);
            __m128 a = _mm_castsi128_ps(_mm_cvtsi32_si128(den[i])), b = _mm_castsi128_ps(_mm_cvtsi32_si128(den[j]));
            unsigned r[4];
            r[0] = _mm_cvtsi128_si32(_mm_castps_si128(_mm_add_ss(a, b)));
            r[1] = _mm_cvtsi128_si32(_mm_castps_si128(_mm_mul_ss(a, b)));
            r[2] = _mm_cvtsi128_si32(_mm_castps_si128(_mm_sub_ss(a, b)));
            r[3] = _mm_cvtsi128_si32(_mm_castps_si128(_mm_sqrt_ss(a)));
            unsigned m = getcsr() & 0x3f;
            unsigned st = _statusfp() & (SW | 0x80000);
            setcsr(0x1f80);
            printf("ftz daz %08x %08x: add %08x mul %08x sub %08x sqrt %08x mxcsr flags %#x _statusfp %#x\n", den[i], den[j], r[0], r[1], r[2], r[3], m, st);
        }
    return 0;
}
