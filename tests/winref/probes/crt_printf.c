/* msvcrt printf family: conversions, flags, widths, the msvcrt spellings of inf/nan, return
 * values of the bounded variants. */
#include "../probe.h"
#include <wchar.h>
#include <math.h>

#define P(...) do { char b[512]; int n = sprintf(b, __VA_ARGS__); printf("%-28s|%s| %d\n", #__VA_ARGS__, b, n); } while (0)

int main(void) {
    static const double d[] = { 0.0, -0.0, 1.0, -1.5, 0.1, 1.0 / 3, 123456789.0, 1e21, 1e-7, 5e-324,
                                1.7976931348623157e308, 2.5, 3.5, 0.125, 9.999999, 1e15, 0.5e-3 };
    static const char *dfmt[] = { "%f", "%.0f", "%.1f", "%.10f", "%e", "%.3e", "%E", "%g", "%.3g", "%G",
                                  "%#g", "%#.0f", "%+f", "% e", "%010.3f", "%-12.4e|", "%.15g", "%.17g" };
    for (unsigned i = 0; i < sizeof d / sizeof *d; i++)
        for (unsigned k = 0; k < sizeof dfmt / sizeof *dfmt; k++) {
            char b[512];
            int n = sprintf(b, dfmt[k], d[i]);
            printf("%s %.17g |%s| %d\n", dfmt[k], d[i], b, n);
        }
    /* infinity and NaN (msvcrt: 1.#INF00, -1.#IND00, 1.#QNAN0) */
    double inf = HUGE_VAL, nan_ = inf - inf, qnan = -nan_;
    P("%f %f %f", inf, -inf, nan_);
    P("%f", qnan);
    P("%e %g %.2f %.0e", inf, inf, inf, inf);
    P("%10.3f|%-10g|", nan_, qnan);
    /* integers */
    static const int iv[] = { 0, 1, -1, 42, -42, 2147483647, (int)0x80000000u, 255 };
    static const char *ifmt[] = { "%d", "%i", "%u", "%x", "%X", "%o", "%#x", "%#o", "%+d", "% d", "%5d", "%-5d|",
                                  "%05d", "%.3d", "%8.3d", "%hd", "%hu", "%hhd", "%c", "%#X", "%.0d", "%lx" };
    for (unsigned i = 0; i < sizeof iv / sizeof *iv; i++)
        for (unsigned k = 0; k < sizeof ifmt / sizeof *ifmt; k++) {
            char b[256];
            int n = sprintf(b, ifmt[k], iv[i]);
            printf("%s %d |%s| %d\n", ifmt[k], iv[i], b, n);
        }
    P("%I64d %I64u %I64x", (long long)-1, (unsigned long long)-1, 0x123456789abcdefull);
    P("%lld %llu", -9223372036854775807ll - 1, 18446744073709551615ull);
    P("%I32d %I32x", -5, 0xdeadbeef);
    P("%*d|%-*d|%.*f", 6, 7, 6, 7, 2, 3.14159);
    P("%*d", -6, 7);
    P("%s|%.2s|%5s|%-5s|", "abc", "abc", "abc", "abc");
    P("%s", (char *)0);
    P("%S", L"wide");
    P("%ls %hs", L"ls", "hs");
    P("%C%c", (wint_t)L'W', 'n');
    P("%%|%5%|");
    P("%p", (void *)0x1234);
    P("%#p", (void *)0x1234);
    P("%y %d", 5);
    { int n = 0; char b[64]; sprintf(b, "abc%n", &n); printf("%%n %d\n", n); }
    /* bounded variants */
    {
        char b[8];
        memset(b, 'x', sizeof b);
        int n = _snprintf(b, 4, "%s", "abcdefgh");
        printf("_snprintf %d %.8s\n", n, b);
        memset(b, 'x', sizeof b);
        n = _snprintf(b, 4, "%s", "abcd");
        printf("_snprintf exact %d %.8s\n", n, b);
        memset(b, 'x', sizeof b);
        n = _snprintf(b, 4, "%s", "abc");
        printf("_snprintf short %d %.8s\n", n, b);
        n = _scprintf("%d-%s", 1234, "xyz");
        printf("_scprintf %d\n", n);
        wchar_t w[16];
        n = swprintf(w, 16, L"%d %s %S", 7, L"wide", "narrow");
        printf("swprintf %d %ls\n", n, w);
        n = _snwprintf(w, 3, L"%ls", L"abcdef");
        printf("_snwprintf %d\n", n);
    }
    return 0;
}
