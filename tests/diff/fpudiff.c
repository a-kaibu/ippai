/* Differential test of floating point (tests/diff/fpu.py): SSE/SSE2 scalar operations and x87
 * operations on boundary operands (the kinds TestFloat picks: zeros, subnormals, the normal
 * limits, one and its neighbours, the largest finite, infinities, quiet and signalling NaNs,
 * integer limits) and random ones, in every rounding mode, with and without DAZ/FTZ. Each line is
 * one operation: its operands, its result bits and the exception flags it raised (MXCSR or the
 * x87 status word). The reference is the real CPU (the program under Wine runs natively).
 *
 *   fpudiff.exe <seed> <random count>
 */
#include <windows.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>

static char obuf[1 << 16];
static int olen;
static void oflush(void) {
    DWORD n;
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), obuf, olen, &n, NULL);
    olen = 0;
}
static void out(const char *fmt, ...) {
    va_list va;
    if (olen > (int)sizeof(obuf) - 1024) oflush();
    va_start(va, fmt);
    olen += vsprintf(obuf + olen, fmt, va);
    va_end(va);
}

static unsigned long long rs;
static unsigned long long rnd(void) {
    rs ^= rs << 13; rs ^= rs >> 7; rs ^= rs << 17;
    return rs;
}

static const unsigned long long d_edge[] = {
    0, 0x8000000000000000ull, 1, 0x800fffffffffffffull, 0x000fffffffffffffull, 0x0010000000000000ull,
    0x8010000000000000ull, 0x3ff0000000000000ull, 0xbff0000000000000ull, 0x3fefffffffffffffull,
    0x3ff0000000000001ull, 0x7fefffffffffffffull, 0xffefffffffffffffull, 0x7ff0000000000000ull,
    0xfff0000000000000ull, 0x7ff8000000000000ull, 0xfff8000000000001ull, 0x7ff0000000000001ull,
    0x7ff4000000000000ull, 0x41e0000000000000ull /* 2^31 */, 0xc1e0000000000000ull, 0x41dfffffffc00000ull,
    0x4340000000000000ull /* 2^53 */, 0x3fe0000000000000ull /* .5 */, 0x3ff8000000000000ull /* 1.5 */,
    0x4004000000000000ull /* 2.5 */, 0xbfe0000000000001ull, 0x0000000000000003ull, 0x3cb0000000000000ull,
    0x7fe0000000000000ull, 0x0008000000000000ull, 0x4330000000000000ull /* 2^52 */,
};
static const unsigned f_edge[] = {
    0, 0x80000000u, 1, 0x807fffffu, 0x007fffffu, 0x00800000u, 0x80800000u, 0x3f800000u, 0xbf800000u,
    0x3f7fffffu, 0x3f800001u, 0x7f7fffffu, 0xff7fffffu, 0x7f800000u, 0xff800000u, 0x7fc00000u,
    0xffc00001u, 0x7f800001u, 0x7fa00000u, 0x4f000000u, 0xcf000000u, 0x3f000000u, 0x3fc00000u,
    0x40200000u, 0x4b800000u, 0x33800000u, 0x7f000000u, 0x00400000u,
};
#define ND (sizeof(d_edge) / sizeof(d_edge[0]))
#define NF (sizeof(f_edge) / sizeof(f_edge[0]))

static unsigned long long rand_d(void) {
    unsigned long long r = rnd();
    switch (rnd() % 6) {
    case 0: return d_edge[r % ND];
    case 1: return (r & 0x800fffffffffffffull) | ((rnd() % 4) << 52) ;            /* tiny */
    case 2: return (r & 0x800fffffffffffffull) | ((0x7fc + rnd() % 3) << 52);     /* huge */
    case 3: return (r & 0x800fffffffffffffull) | ((0x3fd + rnd() % 5) << 52);     /* near 1 */
    case 4: return d_edge[r % ND] ^ (rnd() % 4);                                   /* next to an edge */
    default: return r;
    }
}
static unsigned rand_f(void) {
    unsigned r = (unsigned)rnd();
    switch (rnd() % 5) {
    case 0: return f_edge[r % NF];
    case 1: return (r & 0x807fffffu) | ((unsigned)(rnd() % 3) << 23);
    case 2: return (r & 0x807fffffu) | ((unsigned)(0xfd + rnd() % 2) << 23);
    case 3: return f_edge[r % NF] ^ (unsigned)(rnd() % 4);
    default: return r;
    }
}

/* --- SSE: run with MXCSR = mode, flags cleared; print the result and the new flags */
static unsigned mx_mode;
#define SSE_BEGIN unsigned mx = mx_mode, after; __asm__ volatile("ldmxcsr %0" :: "m"(mx));
#define SSE_END __asm__ volatile("stmxcsr %0" : "=m"(after)); after &= 0x3f;

#define SD_OP2(name, insn) \
    static void name(unsigned long long a, unsigned long long b) { \
        unsigned long long r; SSE_BEGIN \
        __asm__ volatile("movsd %1, %%xmm0\n\t" insn " %2, %%xmm0\n\tmovsd %%xmm0, %0" : "=m"(r) : "m"(a), "m"(b) : "xmm0"); \
        SSE_END out("%s %08x %016llx %016llx -> %016llx %02x\n", #name, mx_mode, a, b, r, after); }
SD_OP2(addsd, "addsd") SD_OP2(subsd, "subsd") SD_OP2(mulsd, "mulsd") SD_OP2(divsd, "divsd")
SD_OP2(minsd, "minsd") SD_OP2(maxsd, "maxsd")
static void sqrtsd(unsigned long long a) {
    unsigned long long r; SSE_BEGIN
    __asm__ volatile("sqrtsd %1, %%xmm0\n\tmovsd %%xmm0, %0" : "=m"(r) : "m"(a) : "xmm0");
    SSE_END out("sqrtsd %08x %016llx -> %016llx %02x\n", mx_mode, a, r, after);
}
#define SS_OP2(name, insn) \
    static void name(unsigned a, unsigned b) { \
        unsigned r; SSE_BEGIN \
        __asm__ volatile("movss %1, %%xmm0\n\t" insn " %2, %%xmm0\n\tmovss %%xmm0, %0" : "=m"(r) : "m"(a), "m"(b) : "xmm0"); \
        SSE_END out("%s %08x %08x %08x -> %08x %02x\n", #name, mx_mode, a, b, r, after); }
SS_OP2(addss, "addss") SS_OP2(subss, "subss") SS_OP2(mulss, "mulss") SS_OP2(divss, "divss")
SS_OP2(minss, "minss") SS_OP2(maxss, "maxss")
static void sqrtss(unsigned a) {
    unsigned r; SSE_BEGIN
    __asm__ volatile("sqrtss %1, %%xmm0\n\tmovss %%xmm0, %0" : "=m"(r) : "m"(a) : "xmm0");
    SSE_END out("sqrtss %08x %08x -> %08x %02x\n", mx_mode, a, r, after);
}
static void cvts(unsigned long long a, unsigned f) {
    unsigned r32; unsigned long long r64; int i1, i2, i3, i4;
    { SSE_BEGIN __asm__ volatile("cvtsd2ss %1, %%xmm0\n\tmovss %%xmm0, %0" : "=m"(r32) : "m"(a) : "xmm0"); SSE_END
      out("cvtsd2ss %08x %016llx -> %08x %02x\n", mx_mode, a, r32, after); }
    { SSE_BEGIN __asm__ volatile("cvtss2sd %1, %%xmm0\n\tmovsd %%xmm0, %0" : "=m"(r64) : "m"(f) : "xmm0"); SSE_END
      out("cvtss2sd %08x %08x -> %016llx %02x\n", mx_mode, f, r64, after); }
    { SSE_BEGIN __asm__ volatile("cvtsd2si %1, %0" : "=r"(i1) : "m"(a)); SSE_END
      out("cvtsd2si %08x %016llx -> %08x %02x\n", mx_mode, a, i1, after); }
    { SSE_BEGIN __asm__ volatile("cvttsd2si %1, %0" : "=r"(i2) : "m"(a)); SSE_END
      out("cvttsd2si %08x %016llx -> %08x %02x\n", mx_mode, a, i2, after); }
    { SSE_BEGIN __asm__ volatile("cvtss2si %1, %0" : "=r"(i3) : "m"(f)); SSE_END
      out("cvtss2si %08x %08x -> %08x %02x\n", mx_mode, f, i3, after); }
    { SSE_BEGIN __asm__ volatile("cvttss2si %1, %0" : "=r"(i4) : "m"(f)); SSE_END
      out("cvttss2si %08x %08x -> %08x %02x\n", mx_mode, f, i4, after); }
    { int k = (int)(a >> 20); SSE_BEGIN __asm__ volatile("cvtsi2sd %1, %%xmm0\n\tmovsd %%xmm0, %0" : "=m"(r64) : "m"(k) : "xmm0"); SSE_END
      out("cvtsi2sd %08x %08x -> %016llx %02x\n", mx_mode, k, r64, after);
      { SSE_BEGIN __asm__ volatile("cvtsi2ss %1, %%xmm0\n\tmovss %%xmm0, %0" : "=m"(r32) : "m"(k) : "xmm0"); SSE_END
        out("cvtsi2ss %08x %08x -> %08x %02x\n", mx_mode, k, r32, after); } }
}
static void comis(unsigned long long a, unsigned long long b) {
    unsigned fl1, fl2;
    { SSE_BEGIN __asm__ volatile("movsd %1, %%xmm0\n\tcomisd %2, %%xmm0\n\tpushf\n\tpop %0" : "=r"(fl1) : "m"(a), "m"(b) : "xmm0", "cc"); SSE_END
      out("comisd %08x %016llx %016llx -> %03x %02x\n", mx_mode, a, b, fl1 & 0x8d5, after); }
    { SSE_BEGIN __asm__ volatile("movsd %1, %%xmm0\n\tucomisd %2, %%xmm0\n\tpushf\n\tpop %0" : "=r"(fl2) : "m"(a), "m"(b) : "xmm0", "cc"); SSE_END
      out("ucomisd %08x %016llx %016llx -> %03x %02x\n", mx_mode, a, b, fl2 & 0x8d5, after); }
}

/* --- x87: control word = precision/rounding, exceptions masked; result stored as a double
 * (fstp m64 rounds once more: the double is what programs see) and as the 80-bit register */
static unsigned short x87_cw;
#define X87_OP2(name, insn) \
    static void name(unsigned long long a, unsigned long long b) { \
        unsigned long long r; unsigned short sw; unsigned char t[10]; \
        __asm__ volatile("fninit\n\tfldcw %4\n\tfldl %2\n\tfldl %3\n\t" insn "\n\tfld %%st(0)\n\tfstpt %1\n\tfstpl %0\n\tfnstsw %%ax\n\tmovw %%ax, %5\n\tfninit" \
                         : "=m"(r), "=m"(t) : "m"(a), "m"(b), "m"(x87_cw), "m"(sw) : "ax", "st", "st(1)"); \
        out("%s %04x %016llx %016llx -> %016llx %02x%02x%02x%02x%02x%02x%02x%02x%02x%02x %02x\n", #name, x87_cw, a, b, r, \
            t[9], t[8], t[7], t[6], t[5], t[4], t[3], t[2], t[1], t[0], sw & 0x3f); }
X87_OP2(fadd, "faddp") X87_OP2(fsub, "fsubp") X87_OP2(fmul, "fmulp") X87_OP2(fdiv, "fdivp")
X87_OP2(fdivr, "fdivrp") X87_OP2(fprem, "fprem\n\tfstp %%st(1)") X87_OP2(fscale, "fxch\n\tfscale\n\tfstp %%st(1)")
#define X87_OP1(name, insn) \
    static void name(unsigned long long a) { \
        unsigned long long r; unsigned short sw; unsigned char t[10]; \
        __asm__ volatile("fninit\n\tfldcw %3\n\tfldl %2\n\t" insn "\n\tfld %%st(0)\n\tfstpt %1\n\tfstpl %0\n\tfnstsw %%ax\n\tmovw %%ax, %4\n\tfninit" \
                         : "=m"(r), "=m"(t) : "m"(a), "m"(x87_cw), "m"(sw) : "ax", "st", "st(1)"); \
        out("%s %04x %016llx -> %016llx %02x%02x%02x%02x%02x%02x%02x%02x%02x%02x %02x\n", #name, x87_cw, a, r, \
            t[9], t[8], t[7], t[6], t[5], t[4], t[3], t[2], t[1], t[0], sw & 0x3f); }
X87_OP1(fsqrt, "fsqrt") X87_OP1(frndint, "frndint") X87_OP1(fabs_, "fabs") X87_OP1(fchs, "fchs")
X87_OP1(fsin, "fsin") X87_OP1(fcos, "fcos") X87_OP1(fptan, "fptan\n\tfstp %%st(0)") X87_OP1(f2xm1, "f2xm1")
X87_OP1(fyl2x, "fld1\n\tfxch\n\tfyl2x") X87_OP1(fpatan, "fld1\n\tfpatan")
static void fist(unsigned long long a) {
    int i; long long q; short h; unsigned short sw;
    __asm__ volatile("fninit\n\tfldcw %4\n\tfldl %3\n\tfld %%st(0)\n\tfld %%st(0)\n\tfistpl %0\n\tfistpll %1\n\tfistps %2\n\tfnstsw %%ax\n\tmovw %%ax, %5\n\tfninit"
                     : "=m"(i), "=m"(q), "=m"(h) : "m"(a), "m"(x87_cw), "m"(sw) : "ax", "st", "st(1)", "st(2)");
    out("fist %04x %016llx -> %08x %016llx %04hx %02x\n", x87_cw, a, i, q, h, sw & 0x3f);
}
static void fcom(unsigned long long a, unsigned long long b) {
    unsigned short sw1, sw2; unsigned fl;
    __asm__ volatile("fninit\n\tfldl %3\n\tfldl %2\n\tfcomp %%st(1)\n\tfnstsw %%ax\n\tmovw %%ax, %0\n\tfninit" : "=m"(sw1) : "m"(x87_cw), "m"(a), "m"(b) : "ax", "st", "st(1)");
    __asm__ volatile("fninit\n\tfldl %3\n\tfldl %2\n\tfucomip %%st(1)\n\tpushf\n\tpop %%eax\n\tmovl %%eax, %1\n\tfnstsw %%ax\n\tmovw %%ax, %0\n\tfninit" : "=m"(sw2), "=m"(fl) : "m"(a), "m"(b) : "eax", "st", "st(1)", "cc");
    out("fcom %016llx %016llx -> %04x %04x %03x\n", a, b, sw1 & 0x473f, sw2 & 0x3f, fl & 0x8d5);
}

int main(int argc, char **argv) {
    static const unsigned modes[] = { 0x1f80, 0x3f80, 0x5f80, 0x7f80, 0x9fc0 /* FTZ+DAZ */ };
    static const unsigned short cws[] = { 0x027f /* PC53 near: Windows */, 0x067f, 0x0a7f, 0x0e7f, 0x037f /* PC64 */, 0x007f /* PC24 */ };
    unsigned seed = argc > 1 ? (unsigned)atoi(argv[1]) : 1;
    int count = argc > 2 ? atoi(argv[2]) : 2000, i, j, m;
    rs = 0x9e3779b97f4a7c15ull * (seed + 1);
    /* every pair of edge operands, every mode */
    for (m = 0; m < 5; m++) {
        mx_mode = modes[m];
        for (i = 0; i < (int)ND; i++) {
            sqrtsd(d_edge[i]);
            cvts(d_edge[i], f_edge[i % NF]);
            for (j = 0; j < (int)ND; j++) {
                addsd(d_edge[i], d_edge[j]); subsd(d_edge[i], d_edge[j]); mulsd(d_edge[i], d_edge[j]);
                divsd(d_edge[i], d_edge[j]); minsd(d_edge[i], d_edge[j]); maxsd(d_edge[i], d_edge[j]);
                if (m == 0) comis(d_edge[i], d_edge[j]);
            }
        }
        for (i = 0; i < (int)NF; i++) {
            sqrtss(f_edge[i]);
            for (j = 0; j < (int)NF; j++) {
                addss(f_edge[i], f_edge[j]); subss(f_edge[i], f_edge[j]); mulss(f_edge[i], f_edge[j]);
                divss(f_edge[i], f_edge[j]); minss(f_edge[i], f_edge[j]); maxss(f_edge[i], f_edge[j]);
            }
        }
    }
    for (m = 0; m < 6; m++) {
        x87_cw = cws[m];
        for (i = 0; i < (int)ND; i++) {
            fsqrt(d_edge[i]); frndint(d_edge[i]); fabs_(d_edge[i]); fchs(d_edge[i]); fist(d_edge[i]);
            if (m == 0) { fsin(d_edge[i]); fcos(d_edge[i]); fptan(d_edge[i]); f2xm1(d_edge[i]); fyl2x(d_edge[i]); fpatan(d_edge[i]); }
            for (j = 0; j < (int)ND; j++) {
                fadd(d_edge[i], d_edge[j]); fsub(d_edge[i], d_edge[j]); fmul(d_edge[i], d_edge[j]);
                fdiv(d_edge[i], d_edge[j]); fdivr(d_edge[i], d_edge[j]);
                if (m == 0) { fprem(d_edge[i], d_edge[j]); fscale(d_edge[i], d_edge[j]); fcom(d_edge[i], d_edge[j]); }
            }
        }
    }
    /* random operands */
    for (i = 0; i < count; i++) {
        unsigned long long a = rand_d(), b = rand_d();
        unsigned fa = rand_f(), fb = rand_f();
        mx_mode = modes[rnd() % 5];
        addsd(a, b); subsd(a, b); mulsd(a, b); divsd(a, b); sqrtsd(a); cvts(a, fa); comis(a, b);
        addss(fa, fb); mulss(fa, fb); divss(fa, fb); sqrtss(fa);
        x87_cw = cws[rnd() % 6];
        fadd(a, b); fmul(a, b); fdiv(a, b); fsqrt(a); fist(a); frndint(a); fcom(a, b);
    }
    oflush();
    return 0;
}
