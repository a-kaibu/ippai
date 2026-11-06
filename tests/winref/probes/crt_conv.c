/* msvcrt string <-> number conversions: strtod/atof rounding and syntax, strtol bases and
 * overflow (errno), sscanf, _ecvt/_fcvt/_gcvt, _itoa family. */
#include "../probe.h"
#include <errno.h>
#include <limits.h>

int main(void) {
    static const char *ds[] = { "0", "1", "-1.5", "0.1", "3.14159265358979323846", "1e308", "1e309", "-1e309",
        "1e-320", "1e-400", "2.2250738585072011e-308", "4.9406564584124654e-324", "123abc", "  +.5e2x",
        "0x1p3", "inf", "nan", "1.#INF", "  12.5e", ".e1", "1e+", "9007199254740993", "0.000000000000000000001",
        "179769313486231580793728971405303415079934132710037826936173778980444968292764750946649017977587207096330286416692887910946555547851940402630657488671505820681908902000708383676273854845817711531764475730270069855571366959622842914819860834936475292719074168444365510704342711559699508093042880177904174497791", "1,5", "D1" };
    for (unsigned i = 0; i < sizeof ds / sizeof *ds; i++) {
        char *end;
        errno = 0;
        double v = strtod(ds[i], &end);
        char label[64];
        sprintf(label, "strtod[%u]", i);
        probe_ulp(0, label, v);
        printf("strtod[%u] end %d errno %d\n", i, (int)(end - ds[i]), errno);
        probe_ulp(0, "atof", atof(ds[i]));
    }
    static const char *ls[] = { "0", "123", "-123", "2147483647", "2147483648", "-2147483648", "-2147483649",
        "0x1A", "0X1a", "077", "  +42z", "-0x", "zz", "4294967295", "4294967296", "99999999999", "  -", "0b101" };
    static const int bases[] = { 0, 10, 16, 8, 2, 36 };
    for (unsigned i = 0; i < sizeof ls / sizeof *ls; i++)
        for (unsigned k = 0; k < sizeof bases / sizeof *bases; k++) {
            char *e1, *e2;
            errno = 0;
            long l = strtol(ls[i], &e1, bases[k]);
            int er1 = errno;
            errno = 0;
            unsigned long u = strtoul(ls[i], &e2, bases[k]);
            printf("strtol '%s' %d: %ld %d %d | %lu %d %d\n", ls[i], bases[k], l, (int)(e1 - ls[i]), er1,
                   u, (int)(e2 - ls[i]), errno);
        }
    for (unsigned i = 0; i < sizeof ls / sizeof *ls; i++)
        printf("atoi '%s' %d atol %ld _atoi64 %I64d\n", ls[i], atoi(ls[i]), atol(ls[i]), _atoi64(ls[i]));
    /* sscanf */
    {
        int a = -1, b = -1, n = -1; char s[32] = "-"; double f = 0; unsigned x = 0; char c = '-';
        int r = sscanf("12 34 hello 2.5 ff Z", "%d %d %31s %lf %x %c%n", &a, &b, s, &f, &x, &c, &n);
        printf("sscanf %d: %d %d %s %g %x %c %d\n", r, a, b, s, f, x, c, n);
        r = sscanf("abc", "%d", &a);
        printf("sscanf nomatch %d\n", r);
        r = sscanf("", "%d", &a);
        printf("sscanf empty %d\n", r);
        r = sscanf("key=value;rest", "%[^=]=%[^;]", s, s + 16);
        printf("sscanf set %d %s %s\n", r, s, s + 16);
        r = sscanf("  42", "%3d", &a);
        printf("sscanf width %d %d\n", r, a);
        r = sscanf("123456", "%2d%3d", &a, &b);
        printf("sscanf widths %d %d %d\n", r, a, b);
    }
    /* ecvt/fcvt/gcvt */
    static const double cv[] = { 0, 1, -1.5, 123.456, 0.000123, 1e20, 9.995, 0.5, 2.5 };
    for (unsigned i = 0; i < sizeof cv / sizeof *cv; i++) {
        int dec, sign;
        char *e = _ecvt(cv[i], 6, &dec, &sign);
        printf("_ecvt %g: %s %d %d\n", cv[i], e, dec, sign);
        e = _fcvt(cv[i], 3, &dec, &sign);
        printf("_fcvt %g: %s %d %d\n", cv[i], e, dec, sign);
        char g[64];
        printf("_gcvt %g: %s\n", cv[i], _gcvt(cv[i], 5, g));
    }
    /* itoa family */
    static const int radix[] = { 2, 8, 10, 16, 36 };
    static const int vals[] = { 0, 1, -1, 255, INT_MIN, INT_MAX, -12345 };
    for (unsigned i = 0; i < sizeof vals / sizeof *vals; i++)
        for (unsigned k = 0; k < sizeof radix / sizeof *radix; k++) {
            char a[80], b[80], c[80];
            printf("itoa %d r%d: %s %s %s\n", vals[i], radix[k], _itoa(vals[i], a, radix[k]),
                   _ultoa((unsigned long)vals[i], b, radix[k]), _i64toa((long long)vals[i] * 1000003, c, radix[k]));
        }
    return 0;
}
