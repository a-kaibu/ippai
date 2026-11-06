/* msvcrt math: results as bit patterns (within an ulp where the CRT may round differently),
 * plus errno and the special cases. */
#include "../probe.h"
#include <math.h>
#include <float.h>
#include <errno.h>

static const double xs[] = { 0.0, -0.0, 0.5, 1.0, -1.0, 2.0, 3.14159265358979, 1e-10, 10.0, 100.5, -7.25,
                             700.0, 1e300, 0.7853981633974483, 1e10, 4.9e-324 };

#define F1(fn, tol) for (unsigned i = 0; i < sizeof xs / sizeof *xs; i++) { \
        char l[64]; errno = 0; double r = fn(xs[i]); sprintf(l, #fn "(%.17g)e%d", xs[i], errno == 0 ? 0 : 1); probe_ulp(tol, l, r); }
#define F2(fn, tol) for (unsigned i = 0; i < sizeof xs / sizeof *xs; i++) for (unsigned j = 0; j < sizeof xs / sizeof *xs; j += 3) { \
        char l[96]; errno = 0; double r = fn(xs[i], xs[j]); sprintf(l, #fn "(%.17g,%.17g)", xs[i], xs[j]); probe_ulp(tol, l, r); }

int main(void) {
    F1(sqrt, 0) F1(floor, 0) F1(ceil, 0) F1(fabs, 0)
    F1(sin, 1) F1(cos, 1) F1(tan, 1) F1(asin, 1) F1(acos, 1) F1(atan, 1) F1(exp, 1) F1(log, 1) F1(log10, 1)
    F1(sinh, 2) F1(cosh, 2) F1(tanh, 2)
    F2(pow, 1) F2(atan2, 1) F2(fmod, 0) F2(_hypot, 1)
    for (unsigned i = 0; i < sizeof xs / sizeof *xs; i++) {
        int e;
        double ip, m = frexp(xs[i], &e), f = modf(xs[i], &ip);
        char l[64];
        sprintf(l, "frexp(%.17g)%d", xs[i], e); probe_ulp(0, l, m);
        sprintf(l, "modf(%.17g)", xs[i]); probe_ulp(0, l, f); probe_ulp(0, l, ip);
        printf("_isnan %d _finite %d _fpclass %d\n", _isnan(xs[i]), _finite(xs[i]), _fpclass(xs[i]));
    }
    errno = 0; log(-1.0); printf("log(-1) errno %d\n", errno);
    errno = 0; sqrt(-1.0); printf("sqrt(-1) errno %d\n", errno);
    errno = 0; exp(1000.0); printf("exp(1000) errno %d\n", errno);
    errno = 0; pow(0.0, -1.0); printf("pow(0,-1) errno %d\n", errno);
    /* x87 control word as msvcrt sets it, and float->int */
    printf("_controlfp %#x\n", _controlfp(0, 0) & (_MCW_PC | _MCW_RC | _MCW_EM));
    static const double cv[] = { 2.5, 3.5, -2.5, -0.5, 1e10, -1e10, 2147483647.9, -2147483648.9 };
    for (unsigned i = 0; i < sizeof cv / sizeof *cv; i++)
        printf("(int)%.17g = %d (long long) %I64d\n", cv[i], (int)cv[i], (long long)cv[i]);
    printf("abs %d labs %ld div %d %d\n", abs(-5), labs(-7L), div(-7, 2).quot, div(-7, 2).rem);
    /* _controlfp's rounding modes as fistp/frndint (x87) and cvtsd2si (SSE) see them, its
     * precision control, and what _control87/_controlfp_s report */
    static const unsigned rcs[] = { _RC_NEAR, _RC_DOWN, _RC_UP, _RC_CHOP };
    static const double rv[] = { 2.5, 3.5, -2.5, -1.7, 1.2 };
    for (unsigned m = 0; m < 4; m++) {
        unsigned r = _controlfp(rcs[m], _MCW_RC);
        printf("rc %u: _controlfp %#x _control87 %#x", m, r & (_MCW_PC | _MCW_RC | _MCW_EM), _control87(0, 0) & (_MCW_PC | _MCW_RC | _MCW_EM));
        for (unsigned i = 0; i < sizeof rv / sizeof *rv; i++) {
            volatile double v = rv[i];
#ifdef __i386__
            int n; double f;
            __asm__ volatile("fldl %1\n\tfistpl %0" : "=m"(n) : "m"(v));
            __asm__ volatile("fldl %1\n\tfrndint\n\tfstpl %0" : "=m"(f) : "m"(v));
            printf(" %g:%d/%g", v, n, f);
#endif
            int s2;
            __asm__ volatile("cvtsd2si %1, %0" : "=r"(s2) : "x"(v));
            printf(" sse %d", s2);
        }
        printf("\n");
    }
    _controlfp(_RC_NEAR, _MCW_RC);
    unsigned cur = 0;
    int e = _controlfp_s(&cur, _EM_ZERODIVIDE, _EM_ZERODIVIDE);
    printf("_controlfp_s %d %#x\n", e, cur & (_MCW_PC | _MCW_RC | _MCW_EM));
#ifdef __i386__
    static const unsigned pcs[] = { _PC_24, _PC_53, _PC_64 };
    for (unsigned m = 0; m < 3; m++) {
        unsigned r = _controlfp(pcs[m], _MCW_PC);
        unsigned short cw;
        __asm__ volatile("fnstcw %0" : "=m"(cw));
        volatile double one = 1.0, three = 3.0, third = one / three;
        printf("pc %u: _controlfp %#x fnstcw %#x\n", m, r & (_MCW_PC | _MCW_RC | _MCW_EM), cw);
        printf("pc %u: 1/3 %.17g\n", m, third);
    }
    _controlfp(_PC_53, _MCW_PC);
#endif
    return 0;
}
