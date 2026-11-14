/* x87 floating point in a 16-bit program built for the emulator (-fpi): the file holds real x87
 * code with OSFIXUP records (FIDRQQ, FIWRQQ, FIERQQ, FICRQQ...) that Windows applies only
 * without a coprocessor, and imports WIN87EM. Arithmetic, the math library, conversions,
 * comparisons and printf's formatting of doubles. (#37)
 * libs: */
#include <windows.h>
#include <stdio.h>
#include <math.h>
#include <float.h>

static volatile double a = 2.0, b = 3.5, z = 0.0;
static volatile float f = 1.25f;
static double far fd = 0.1;

int PASCAL WinMain(HINSTANCE hi, HINSTANCE hp, LPSTR cmd, int sh) {
    long l;
    int i;
    double s = 0;
    printf("%.6f %.6f %.6f %.6f\n", a + b, a - b, a * b, a / b);
    printf("%.10f %.10f %.10f\n", sqrt(b), sin(a), cos(a));
    printf("%.10f %.10f %.10f\n", exp(a), log(b), pow(b, a));
    printf("%.10f %.10f\n", atan2(a, b), fmod(b * 7, a));
    printf("%.3e %g %g\n", 1.0e300 * b, f * 3, floor(-b));
    l = (long)(b * 1000000.0);
    i = (int)-b;
    printf("%ld %d %u\n", l, i, (unsigned)(a * 30000));
    printf("%d %d %d\n", a < b, a == 2.0, b >= 4.0);
    for (i = 0; i < 10; i++) s += fd;
    printf("%.15g %08lx%08lx\n", s, ((unsigned long far *)&s)[1], ((unsigned long far *)&s)[0]);
    printf("far %g\n", fd * 10);
    _clear87();
    printf("status after clear %x\n", _status87() & 0x3f);
    return 0;
}
