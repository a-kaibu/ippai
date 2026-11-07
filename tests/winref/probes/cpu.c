/* The CPU itself, as koji's JIT and interpreter must reproduce it: thousands of pseudo-random
 * register-form instructions (integer ALU, shifts and rotates by immediate and by CL, double
 * shifts, multiplies, bit tests and scans, BCD adjusts, CMOV/SETcc, XADD/CMPXCHG, SSE/SSE2
 * integer and float, SSSE3, SSE4.1) on random registers and flags, each run once from
 * generated code. On the Windows runner this is the real processor (crates/koji's diff_native
 * tests do the same against the host CPU, but only for the interpreter). Prints every general
 * register, the defined flags and the changed XMM registers after each one. arch: 32 */
#include "../probe.h"

static unsigned rng = 0x2468ace1;
static unsigned rnd(void) { rng ^= rng << 13; rng ^= rng >> 17; rng ^= rng << 5; return rng; }

/* the state an instruction starts from and ends with */
static struct st { DWORD r[8]; DWORD fl; DWORD pad[3]; BYTE x[8][16]; } in, out;
static DWORD saved_esp;

/* condition flags */
#define CF 0x001
#define PF 0x004
#define AF 0x010
#define ZF 0x040
#define SF 0x080
#define OF 0x800
#define ALL (CF | PF | AF | ZF | SF | OF)

static BYTE *code, *at;
static void b(int v) { *at++ = (BYTE)v; }
static void d(DWORD v) { memcpy(at, &v, 4); at += 4; }

/* generated: pushad, the state in, the instruction, the state out, popad, ret */
static void (*harness(const BYTE *insn, int n))(void) {
    BYTE *start = at;
    b(0x60);
    b(0x89); b(0x25); d((DWORD)&saved_esp);
    for (int i = 0; i < 8; i++) { b(0xf3); b(0x0f); b(0x6f); b(0x05 + i * 8); d((DWORD)in.x[i]); }
    b(0xff); b(0x35); d((DWORD)&in.fl); b(0x9d);
    for (int r = 0; r < 8; r++) if (r != 4) { b(0x8b); b(0x05 + r * 8); d((DWORD)&in.r[r]); }
    memcpy(at, insn, n); at += n;
    b(0x9c); b(0x8f); b(0x05); d((DWORD)&out.fl);
    for (int r = 0; r < 8; r++) if (r != 4) { b(0x89); b(0x05 + r * 8); d((DWORD)&out.r[r]); }
    for (int i = 0; i < 8; i++) { b(0xf3); b(0x0f); b(0x7f); b(0x05 + i * 8); d((DWORD)out.x[i]); }
    b(0x8b); b(0x25); d((DWORD)&saved_esp);
    b(0x61); b(0xc3);
    return (void (*)(void))start;
}

static int gpr(void) { int r; do r = rnd() & 7; while (r == 4); return r; }

static void random_state(void) {
    for (int r = 0; r < 8; r++) {
        unsigned v = rnd();
        /* small values and edges now and then: carries, zero results, sign changes */
        switch (rnd() % 8) {
        case 0: v &= 0xff; break;
        case 1: v = 0x7fffffff + (rnd() % 3); break;
        case 2: v = 0xffffffff - (rnd() % 2); break;
        case 3: v &= 0x8000000f; break;
        }
        in.r[r] = v;
    }
    in.r[4] = 0;
    in.fl = 0x202 | (rnd() & ALL);
    for (int i = 0; i < 8; i++) for (int k = 0; k < 16; k++) in.x[i][k] = rnd();
    /* float lanes: ordinary numbers mostly (the random bytes are NaNs and huge values), built
     * from bits so that no FPU rounding goes into the inputs */
    for (int i = 0; i < 8; i++) if (rnd() % 2) {
        int dbl = rnd() % 2;
        for (int l = 0; l < (dbl ? 2 : 4); l++) {
            int e = rnd() % 10;
            if (dbl) {
                unsigned long long v = (unsigned long long)(rnd() & 0x800fffff) << 32 | rnd();
                v |= (unsigned long long)(1013 + rnd() % 20) << 52;
                if (e == 0) v &= 1ULL << 63;
                if (e == 1) v = (v & ~(0x7ffULL << 52)) | 0x7e0ULL << 52;
                memcpy(in.x[i] + 8 * l, &v, 8);
            } else {
                unsigned v = (rnd() & 0x807fffff) | (120 + rnd() % 20) << 23;
                if (e == 0) v &= 1u << 31;
                if (e == 1) v = 0x3eaaaaab;
                memcpy(in.x[i] + 4 * l, &v, 4);
            }
        }
    }
}

/* an instruction: its bytes and the flags it defines (the rest are left to the CPU) */
struct ins { BYTE b[16]; int n; DWORD flags; int sse; };

static void op(struct ins *x, const char *hex) { while (*hex) { unsigned v; sscanf(hex, "%2x", &v); x->b[x->n++] = v; hex += 2; } }
static void modrm(struct ins *x, int reg, int rm) { x->b[x->n++] = 0xc0 | reg << 3 | rm; }

static void gen(struct ins *x) {
    memset(x, 0, sizeof *x);
    x->flags = ALL;
    unsigned k = rnd() % 100;
    int r = gpr(), m = gpr();
    if (k < 14) { /* ALU r/m, r (8, 16, 32 bits) */
        int o = rnd() % 8, w = rnd() % 3;
        if (w == 1) op(x, "66");
        x->b[x->n++] = o * 8 + (w == 0 ? 0 : 1) + (rnd() % 2) * 2;
        modrm(x, w == 0 ? rnd() % 8 : r, w == 0 ? rnd() % 8 : m);
        if (o == 1 || o == 4 || o == 6) x->flags = ALL & ~AF;
    } else if (k < 22) { /* group 1 with an immediate */
        int o = rnd() % 8, f = rnd() % 3;
        op(x, f == 0 ? "81" : f == 1 ? "83" : "80");
        modrm(x, o, f == 2 ? rnd() % 8 : m);
        int imm = rnd();
        for (int i = 0; i < (f == 0 ? 4 : 1); i++) x->b[x->n++] = imm >> 8 * i;
        if (o == 1 || o == 4 || o == 6) x->flags = ALL & ~AF;
    } else if (k < 34) { /* shifts and rotates: by 1, by an immediate, by CL */
        int o = rnd() % 8, how = rnd() % 3, w = rnd() % 3;
        if (o == 6) o = 4;
        if (w == 1) op(x, "66");
        int base = how == 0 ? 0xd0 : how == 1 ? 0xc0 : 0xd2;
        x->b[x->n++] = base + (w != 0);
        modrm(x, o, w == 0 ? rnd() % 8 : m);
        unsigned cnt = 1;
        if (how == 1) { cnt = rnd() % 40; x->b[x->n++] = cnt; }
        if (how == 2) { in.r[1] = (in.r[1] & ~0xff) | (rnd() % 40); cnt = in.r[1] & 0xff; }
        x->sse = -1 - (int)cnt; /* the count, for the flags (below) */
        x->flags = o; /* resolved after the state is known */
    } else if (k < 40) { /* SHLD/SHRD by an immediate or CL */
        int cl = rnd() % 2, right = rnd() % 2, w16 = rnd() % 4 == 0;
        if (w16) op(x, "66");
        x->b[x->n++] = 0x0f; x->b[x->n++] = (right ? 0xac : 0xa4) + cl;
        modrm(x, r, m);
        unsigned cnt;
        if (!cl) { cnt = rnd() % 32; x->b[x->n++] = cnt; }
        else { if (r == 1 || m == 1) { x->n = 0; return gen(x); } in.r[1] = (in.r[1] & ~0xff) | (rnd() % 32); cnt = in.r[1] & 31; }
        cnt &= 31;
        if (w16 && cnt > 16) { x->n = 0; return gen(x); } /* the result is undefined */
        x->flags = cnt == 0 ? ALL : cnt == 1 ? ALL & ~AF : ALL & ~AF & ~OF;
    } else if (k < 46) { /* multiplies */
        switch (rnd() % 5) {
        case 0: op(x, "f7"); modrm(x, 4 + rnd() % 2, m); break;
        case 1: op(x, "f6"); modrm(x, 4 + rnd() % 2, rnd() % 8); break;
        case 2: op(x, "0faf"); modrm(x, r, m); break;
        case 3: op(x, "6b"); modrm(x, r, m); x->b[x->n++] = rnd(); break;
        default: op(x, "69"); modrm(x, r, m); { DWORD v = rnd(); memcpy(x->b + x->n, &v, 4); x->n += 4; } break;
        }
        x->flags = CF | OF;
    } else if (k < 50) { /* NOT NEG INC DEC */
        int o = rnd() % 4, w = rnd() % 2;
        if (o < 2) { op(x, w ? "f7" : "f6"); modrm(x, 2 + o, w ? m : rnd() % 8); x->flags = o == 0 ? 0 : ALL; }
        else { op(x, w ? "ff" : "fe"); modrm(x, o - 2, w ? m : rnd() % 8); }
    } else if (k < 56) { /* bit tests and scans */
        int o = rnd() % 6;
        if (o < 4) { op(x, "0f"); x->b[x->n++] = 0xa3 + o * 8; modrm(x, r, m); x->flags = CF; }
        else if (o == 4) { op(x, "0fba"); modrm(x, 4 + rnd() % 4, m); x->b[x->n++] = rnd(); x->flags = CF; }
        else { op(x, rnd() % 2 ? "0fbc" : "0fbd"); modrm(x, r, m); x->flags = ZF; if (in.r[m] == 0) in.r[m] = 0x100; }
    } else if (k < 60) { /* BCD, CBW..., carry */
        static const char *o[] = { "27", "2f", "37", "3f", "d40a", "d50a", "d410", "98", "6698", "99", "6699", "f5", "f8", "f9" };
        static const DWORD f[] = { ALL & ~OF, ALL & ~OF, AF | CF, AF | CF, SF | ZF | PF, SF | ZF | PF, SF | ZF | PF, ALL, ALL, ALL, ALL, ALL, ALL, ALL };
        int i = rnd() % 14;
        op(x, o[i]);
        x->flags = f[i];
    } else if (k < 66) { /* CMOVcc, SETcc, MOVZX/MOVSX, BSWAP, XCHG */
        switch (rnd() % 5) {
        case 0: op(x, "0f"); x->b[x->n++] = 0x40 + rnd() % 16; modrm(x, r, m); break;
        case 1: op(x, "0f"); x->b[x->n++] = 0x90 + rnd() % 16; modrm(x, 0, rnd() % 8); break;
        case 2: { int w = rnd() % 2; op(x, "0f"); x->b[x->n++] = 0xb6 + w + (rnd() % 2) * 8; modrm(x, r, w ? m : rnd() % 8); break; } /* (16 bits: not SP) */
        case 3: op(x, "0f"); x->b[x->n++] = 0xc8 + gpr(); break;
        default: op(x, "87"); modrm(x, r, m); break;
        }
    } else if (k < 70) { /* XADD, CMPXCHG, POPCNT */
        switch (rnd() % 3) {
        case 0: op(x, "0fc1"); modrm(x, r, m); break;
        case 1: op(x, "0fb1"); modrm(x, r, m); if (rnd() % 2) in.r[0] = in.r[m]; break;
        default: op(x, "f30fb8"); modrm(x, r, m); break;
        }
    } else { /* SSE/SSE2/SSSE3/SSE4.1 register forms */
        static const char *o[] = {
            /* integer */
            "660ffc", "660ffd", "660ffe", "660fd4", "660ff8", "660ff9", "660ffa", "660ffb", "660fec", "660fed", "660fdc", "660fdd",
            "660fe8", "660fe9", "660fd8", "660fd9", "660fd5", "660fe5", "660fe4", "660ff4", "660ff5", "660ff6", "660fe0", "660fe3",
            "660fda", "660fde", "660fea", "660fee", "660f74", "660f75", "660f76", "660f64", "660f65", "660f66", "660fdb", "660fdf",
            "660feb", "660fef", "660f63", "660f6b", "660f67", "660f60", "660f61", "660f62", "660f6c", "660f68", "660f69", "660f6a",
            "660f6d", "660ff1", "660ff2", "660ff3", "660fd1", "660fd2", "660fd3", "660fe1", "660fe2",
            /* float */
            "0f58", "0f59", "0f5c", "0f5e", "0f5d", "0f5f", "0f51", "f30f58", "f30f59", "f30f5c", "f30f5e", "f30f5d", "f30f5f", "f30f51",
            "660f58", "660f59", "660f5c", "660f5e", "660f5d", "660f5f", "660f51", "f20f58", "f20f59", "f20f5c", "f20f5e", "f20f5d",
            "f20f5f", "f20f51", "0f54", "0f55", "0f56", "0f57", "0f14", "0f15", "660f14", "660f15", "0f5a", "660f5a", "f30f5a", "f20f5a",
            "0f5b", "660f5b", "f30f5b", "660fe6", "f30fe6", "f20fe6", "660f7c", "f20f7c", "660fd0", "f20fd0", "f30f16", "f30f12", "f20f12",
            /* SSSE3, SSE4.1 */
            "660f3800", "660f3801", "660f3802", "660f3803", "660f3804", "660f3805", "660f3806", "660f3807", "660f3808", "660f3809",
            "660f380a", "660f380b", "660f381c", "660f381d", "660f381e", "660f3810", "660f3814", "660f3815", "660f3820", "660f3821",
            "660f3823", "660f3825", "660f3828", "660f3829", "660f382b", "660f3830", "660f3833", "660f3835", "660f3837", "660f3838",
            "660f3839", "660f383a", "660f383b", "660f383c", "660f383d", "660f383e", "660f383f", "660f3840", "660f3841",
            /* with an immediate */
            "660f70/i", "f30f70/i", "f20f70/i", "0fc6/i", "660fc6/i", "0fc2/c", "660fc2/c", "f30fc2/c", "f20fc2/c", "660f3a0f/i", "660f3a0e/i",
            "660f3a0c/i", "660f3a0d/i", "660f3a08/r", "660f3a09/r", "660f3a0a/r", "660f3a0b/r", "660f3a40/d", "660f3a41/d", "660f3a42/i",
            "660f3a21/i",
            /* shifts by an immediate: group 71/72/73 */
            "660f71/g", "660f72/g", "660f73/g",
            /* flags from SSE */
            "0f2e/f", "0f2f/f", "660f2e/f", "660f2f/f", "660f3817/f",
            /* to general registers */
            "660fd7/x", "0f50/x", "660f50/x", "660fc5/x",
        };
        const char *s = o[rnd() % (sizeof o / sizeof *o)];
        char hex[16];
        int n = strcspn(s, "/");
        memcpy(hex, s, n); hex[n] = 0;
        op(x, hex);
        char kind = s[n] ? s[n + 1] : 0;
        int xr = rnd() % 8, xm = rnd() % 8;
        x->sse = 1;
        x->flags = 0;
        switch (kind) {
        case 'g': { int sub = (s[5] == '3') ? (int[]){ 2, 3, 6, 7 }[rnd() % 4] : (int[]){ 2, 4, 6 }[rnd() % 3]; modrm(x, sub, xm); x->b[x->n++] = rnd() % 70; break; }
        case 'f': modrm(x, xr, xm); x->flags = ALL; break;
        case 'x': if (s[4] == 'c') { modrm(x, gpr(), xm); x->b[x->n++] = rnd() % 8; } else modrm(x, gpr(), xm); break;
        case 'c': modrm(x, xr, xm); x->b[x->n++] = rnd() % 8; break;
        case 'r': modrm(x, xr, xm); x->b[x->n++] = rnd() % 16; break;
        case 'd': modrm(x, xr, xm); x->b[x->n++] = (rnd() & 0xf3) | 0x00; break;
        case 'i': modrm(x, xr, xm); x->b[x->n++] = rnd(); break;
        default: modrm(x, xr, xm); break;
        }
    }
}

/* the flags a shift or rotate (group 2, /o) defines, by its count */
static DWORD shift_flags(int o, unsigned cnt, int bits) {
    cnt &= 31;
    if (cnt == 0) return ALL;
    if (o < 4) { /* rotates: only CF and OF change, OF only by 1 */
        if (o >= 2) { /* RCL/RCR rotate through CF: count mod (bits + 1) */
            unsigned c = cnt % (bits + 1);
            if (c == 0) return ALL;
        }
        return cnt == 1 ? ALL : ALL & ~OF;
    }
    DWORD f = ALL & ~AF;
    if (cnt != 1) f &= ~OF;
    if (cnt >= (unsigned)bits && (o == 4 || o == 5)) f &= ~CF; /* SHL/SHR past the width */
    return f;
}

static int width(const struct ins *x) {
    int i = 0;
    int w16 = x->b[0] == 0x66;
    if (w16) i++;
    return (x->b[i] & 1) ? (w16 ? 16 : 32) : 8;
}

int main(void) {
    code = VirtualAlloc(NULL, 4 << 20, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
    at = code;
    int n = 0, sse_n = 0;
    for (int t = 0; t < 4000; t++) {
        random_state();
        struct ins x;
        gen(&x);
        if (x.sse < 0) x.flags = shift_flags(x.flags, -1 - x.sse, width(&x)), x.sse = 0;
        void (*f)(void) = harness(x.b, x.n);
        memset(&out, 0, sizeof out);
        f();
        printf("%04d", t);
        for (int i = 0; i < x.n; i++) printf("%s%02x", i ? "" : " ", x.b[i]);
        printf(":");
        for (int r = 0; r < 8; r++) if (r != 4) printf(" %08lx", out.r[r]);
        printf(" fl %03lx", out.fl & x.flags);
        for (int i = 0; i < 8; i++) if (memcmp(in.x[i], out.x[i], 16)) {
            printf(" x%d ", i);
            for (int k = 15; k >= 0; k--) printf("%02x", out.x[i][k]);
        }
        printf("\n");
        n++;
        sse_n += x.sse;
        if (at - code > (4 << 20) - 512) at = code; /* (never: 4000 harnesses fit) */
    }
    printf("%d instructions, %d SSE\n", n, sse_n);
    return 0;
}
