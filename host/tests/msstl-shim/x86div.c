// x86's 64-bit division helpers, which every x86 MSVC program links from msvcrt.lib (lldiv.obj,
// ulldiv.obj, llrem.obj, ullrem.obj): the arguments on the stack, callee-popped, the result in
// EDX:EAX. (Built by build-msvc64.sh for MSVC_ARCH=i686.)
static unsigned long long udivmod(unsigned long long a, unsigned long long b, unsigned long long *rem) {
    unsigned long long q = 0, r = 0;
    for (int i = 63; i >= 0; i--) {
        r = r << 1 | (a >> i & 1);
        if (r >= b) { r -= b; q |= 1ull << i; }
    }
    *rem = r;
    return q;
}
static unsigned long long mag(long long v) { return v < 0 ? 0 - (unsigned long long)v : (unsigned long long)v; }
unsigned long long __cdecl glue_aulldiv(unsigned long long a, unsigned long long b) { unsigned long long r; return udivmod(a, b, &r); }
unsigned long long __cdecl glue_aullrem(unsigned long long a, unsigned long long b) { unsigned long long r; udivmod(a, b, &r); return r; }
long long __cdecl glue_alldiv(long long a, long long b) {
    unsigned long long r, q = udivmod(mag(a), mag(b), &r);
    return (a < 0) != (b < 0) ? (long long)(0 - q) : (long long)q;
}
long long __cdecl glue_allrem(long long a, long long b) {
    unsigned long long r;
    udivmod(mag(a), mag(b), &r);
    return a < 0 ? (long long)(0 - r) : (long long)r;
}
#define HELPER(n) ".globl __" #n "\n__" #n ":\n pushl 16(%esp)\n pushl 16(%esp)\n pushl 16(%esp)\n pushl 16(%esp)\n call _glue_" #n "\n addl $16, %esp\n ret $16\n"
__asm__(HELPER(aulldiv) HELPER(aullrem) HELPER(alldiv) HELPER(allrem));
