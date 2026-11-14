/* The x87 in its 80-bit format (#52; sake runs it with MASU_X87_80=1, koji's precise x87
 * mode): FADD/FSUB/FMUL/FDIV of m80 values and of m64/m32/int operands, FSQRT, FRNDINT, FST to
 * m64/m32, FISTP m64, FCOMI, FLD/FSTP m80 round trips, chained without storing in between
 * (the 64-bit significand and the 15-bit exponent kept), at precision control 24/53/64 bits and
 * the four rounding modes. Inputs: random 80-bit values of every exponent range (beyond the
 * double's, denormals, zeros, infinities, NaNs). Prints a checksum per precision; they match the
 * same program built for x86-64 Linux and run natively (inline asm, identical instructions).
 * Exceptions stay masked. */
#include <stdio.h>
#include <string.h>

typedef struct { unsigned char b[16]; } X; /* (10 bytes used) */

static unsigned long long seed = 0x1234567;
static unsigned long long rnd(void) { seed = seed * 6364136223846793005ull + 1442695040888963407ull; return seed ^ (seed >> 29); }
static unsigned long long sum;
static void mix(const void *p, int n) {
    const unsigned char *b = p;
    for (int i = 0; i < n; i++) sum = sum * 1099511628211ull ^ b[i];
}

static X value(void) {
    X x;
    memset(&x, 0, sizeof x);
    unsigned long long m = rnd(), r = rnd();
    unsigned e;
    switch (r % 8) {
    case 0: e = 0x3fff + (int)(r >> 8) % 40 - 20; break;     /* around 1 */
    case 1: e = 0x3fff + (int)(r >> 8) % 2400 - 1200; break;  /* beyond the double's range too */
    case 2: e = (r >> 8) % 0x7ffe + 1; break;                 /* anywhere */
    case 3: e = 0; m >>= (r >> 8) % 64; break;                /* denormal (no integer bit) */
    case 4: e = (r >> 9) % 3 ? 0x3fff : 0x7fff; m = (r >> 9) % 3 ? 1ull << 63 : 0xc000000000000000ull | m >> 2; break; /* 1, NaN */
    case 5: e = 0x7fff; m = 1ull << 63; break;                /* infinity */
    case 6: e = 0; m = 0; break;                              /* zero */
    default: e = 0x3fff + (int)(r >> 8) % 128 - 64; m = (m | 1ull << 63) & ~0xfffull; m |= (r >> 20) & 0x801; break; /* near ties */
    }
    if (e != 0 && e != 0x7fff) m |= 1ull << 63;
    if (r & 0x80000000ull) e |= 0x8000;
    memcpy(x.b, &m, 8);
    memcpy(x.b + 8, &e, 2);
    return x;
}

#ifdef __x86_64__
#define POPF "pop %%rax\n\t"
#define AX "rax"
#else
#define POPF "pop %%eax\n\t"
#define AX "eax"
#endif

static void cw(unsigned short v) { __asm__ volatile("fldcw %0" : : "m"(v)); }

/* a op b, then (op b) again and sqrt of the absolute value, all in registers; stored as m80 */
static void run(const X *a, const X *b, const double *d, const float *f, const int *n, unsigned char *o) {
    __asm__ volatile(
        "fldt %[a]\n\tfldt %[b]\n\tfaddp\n\tfldt %[b]\n\tfmulp\n\tfstpt 0(%[o])\n\t"
        "fldt %[a]\n\tfldt %[b]\n\tfsubrp\n\tfldt %[b]\n\tfdivrp\n\tfstpt 16(%[o])\n\t"
        "fldt %[a]\n\tfabs\n\tfsqrt\n\tfldt %[b]\n\tfmulp\n\tfstpt 32(%[o])\n\t"
        "fldt %[a]\n\tfaddl %[d]\n\tfmuls %[f]\n\tfidivl %[n]\n\tfstpt 48(%[o])\n\t"
        "fldt %[a]\n\tfst %%st(1)\n\tfrndint\n\tfstpt 64(%[o])\n\tfstpt 80(%[o])\n\t"
        "fldt %[a]\n\tfstl 96(%[o])\n\tfsts 104(%[o])\n\tfldt %[b]\n\tfcomip %%st(1), %%st\n\tpushf\n\t" POPF "mov %%eax, 108(%[o])\n\t"
        "fistpll 112(%[o])\n\t"
        "fldt %[b]\n\tfldt %[a]\n\tfdivp\n\tfldt %[a]\n\tfsubrp\n\tfstpt 120(%[o])\n\t"
        :
        : [a] "m"(*a), [b] "m"(*b), [d] "m"(*d), [f] "m"(*f), [n] "m"(*n), [o] "r"(o)
        : "memory", AX, "cc", "st", "st(1)", "st(2)", "st(3)", "st(4)", "st(5)", "st(6)", "st(7)");
}

int main(void) {
    static const unsigned short pcs[3] = { 0x000, 0x200, 0x300 };
    for (int p = 0; p < 3; p++) {
        sum = 0;
        for (int rc = 0; rc < 4; rc++) {
            cw(0x3f | pcs[p] | rc << 10);
            for (int i = 0; i < 3000; i++) {
                X a = value(), b = value();
                /* (from bits: computing them would round by the control word on x86) */
                unsigned long long db = rnd() >> 2 | (rnd() & 1) << 63, fb = rnd() >> 34 | (rnd() & 1) << 31;
                unsigned fb32 = (unsigned)fb;
                double d;
                float f;
                memcpy(&d, &db, 8);
                memcpy(&f, &fb32, 4);
                int n = (int)rnd() >> (rnd() % 31);
                unsigned char o[136];
                memset(o, 0, sizeof o);
                run(&a, &b, &d, &f, &n, o);
                /* (the 6 padding bytes of each m80 slot stay zero; the flags: CF ZF PF) */
                o[108] &= 0x45; o[109] = o[110] = o[111] = 0;
                mix(o, sizeof o);
            }
        }
        cw(0x37f);
        printf("pc %d: %016llx\n", p == 0 ? 24 : p == 1 ? 53 : 64, sum);
    }
    return 0;
}
