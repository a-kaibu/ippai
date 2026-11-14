/* Code a runtime emits into PAGE_EXECUTE_READWRITE memory and rewrites later without any
 * protection change or cache flush, the way Mono (Unity) does: after the code ran hot, an
 * immediate is rewritten by a plain store, a byte store, InterlockedExchange, memcpy and
 * WriteProcessMemory; a call site is pointed elsewhere; a trampoline in the exe repoints the
 * call site that called it (and must run once per repointing); a callback rewrites the
 * instruction after its call inside a loop; the memory is freed and code allocated again at
 * the same address; code rewrites its own next instruction. The stores come from functions
 * that ran hot first (as Mono's own code has: translated, not interpreted). Each line prints
 * what the calls returned and "ok" or "STALE". (Each case on its own page: one the
 * interpreter takes over leaves the others translated.) Built for x86 and x64 (its own machine
 * code: a 64-bit Unity's Mono emits the same patterns; calls into the exe go through a stub
 * in the buffer, as rel32 may not reach from there). */
#include <stdio.h>
#include <string.h>
#include <windows.h>

typedef unsigned (*fn)(unsigned);
static unsigned char *buf;

static unsigned run(fn f, int n)
{
    unsigned s = 0;
    for (int i = 0; i < n; i++) s += f(i);
    return s;
}

/* the writers, made hot by warm() on a data buffer */
__attribute__((noinline)) static void poke32(void *p, unsigned v) { *(volatile unsigned *)p = v; }
__attribute__((noinline)) static void poke8(void *p, unsigned char v) { *(volatile unsigned char *)p = v; }
__attribute__((noinline)) static void xchg32(void *p, unsigned v) { InterlockedExchange((volatile LONG *)p, (LONG)v); }
static void *(*volatile copy)(void *, const void *, size_t) = memcpy;
static void warm(void)
{
    static unsigned data[64];
    for (int i = 0; i < 200000; i++) {
        poke32(&data[i & 63], i);
        poke8(&data[i & 63], i);
        xchg32(&data[i & 63], i);
        copy(&data[i & 63], &i, 4);
    }
}

static void put(unsigned char *at, const void *code, size_t n) { copy(at, code, n); }
static void rel32(unsigned char *call, void *target) /* call: the E8 byte */
{
    xchg32(call + 1, (unsigned)((unsigned char *)target - (call + 5)));
}

#ifdef _WIN64
/* a call from the buffer to `target` in the exe: through a stub at `at` (mov rax, imm64;
 * jmp rax) */
static void *via(unsigned char *at, void *target)
{
    unsigned char c[] = { 0x48, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xe0 };
    memcpy(c + 2, &target, 8);
    put(at, c, sizeof c);
    return at;
}
/* caller: sub rsp, 40; call rel32 t; add rsp, 40; ret (the call's E8 at +4) */
static const unsigned char caller_code[] = { 0x48, 0x83, 0xec, 0x28, 0xe8, 0, 0, 0, 0, 0x48, 0x83, 0xc4, 0x28, 0xc3 };
#define CALL_AT 4
#else
static void *via(unsigned char *at, void *target) { return target; }
/* caller: call rel32 t; ret */
static const unsigned char caller_code[] = { 0xe8, 0, 0, 0, 0, 0xc3 };
#define CALL_AT 0
#endif

/* mov eax, imm32; ret */
static void ret_imm(unsigned char *at, unsigned k)
{
    unsigned char c[] = { 0xb8, 0, 0, 0, 0, 0xc3 };
    memcpy(c + 1, &k, 4);
    put(at, c, sizeof c);
}

#define N 100000

static void line(const char *what, unsigned a, unsigned b, unsigned want)
{
    printf("%-22s %u %u %s\n", what, a, b, b == want ? "ok" : "STALE");
}

static void immediates(void)
{
    fn f = (fn)buf;
    unsigned k = 1;
    ret_imm(buf, k);
    unsigned a = run(f, N), b;

    poke32(buf + 1, k = 2);
    b = run(f, N);
    line("imm store", a, b, k * N);

    poke8(buf + 1, k = 3);
    b = run(f, N);
    line("imm byte store", a, b, k * N);

    xchg32(buf + 1, k = 4);
    b = run(f, N);
    line("imm InterlockedExchange", a, b, k * N);

    k = 5;
    copy(buf + 1, &k, 4);
    b = run(f, N);
    line("imm memcpy", a, b, k * N);

    k = 6;
    SIZE_T w = 0;
    BOOL r = WriteProcessMemory(GetCurrentProcess(), buf + 1, &k, 4, &w);
    b = run(f, N);
    printf("WriteProcessMemory %d %lu\n", r, (unsigned long)w);
    line("imm WriteProcessMemory", a, b, k * N);
}

/* caller_code calling t1 (returns 1), then t2 (returns 2) */
static void call_sites(void)
{
    unsigned char *caller = buf + 0x1000, *t1 = buf + 0x1040, *t2 = buf + 0x1080;
    unsigned char *call = caller + CALL_AT;
    put(caller, caller_code, sizeof caller_code);
    ret_imm(t1, 1);
    ret_imm(t2, 2);
    rel32(call, t1);
    unsigned a = run((fn)caller, N);
    xchg32(call + 1, (unsigned)(t2 - (call + 5)));
    unsigned b = run((fn)caller, N);
    line("call site repointed", a, b, 2 * N);
    rel32(call, t1);
    b = run((fn)caller, N);
    line("call site back", a, b, 1 * N);
}

/* Mono's way: a call goes to a trampoline that compiles the target, points the call site at
 * it and calls it; later calls go straight there */
static unsigned char *tramp_target;
static int tramp_runs;
static unsigned __cdecl trampoline(void)
{
    unsigned char *ret = __builtin_return_address(0);
    tramp_runs++;
    xchg32(ret - 4, (unsigned)(tramp_target - ret));
    return ((unsigned (*)(void))tramp_target)();
}

static void trampolines(void)
{
    unsigned char *caller = buf + 0x2000, *t = buf + 0x2040, *call = caller + CALL_AT;
    void *tramp = via(buf + 0x2200, (void *)trampoline);
    put(caller, caller_code, sizeof caller_code);
    ret_imm(t, 3);
    tramp_target = t;
    rel32(call, tramp);
    unsigned a = run((fn)caller, N);
    printf("trampoline runs %d\n", tramp_runs);
    for (int round = 0; round < 3; round++) {
        /* the method compiled again (somewhere else): the call site goes back to the trampoline */
        unsigned char *t2 = buf + 0x2080 + 0x40 * round;
        ret_imm(t2, 10 + round);
        tramp_target = t2;
        rel32(call, tramp);
        unsigned b = run((fn)caller, N);
        printf("trampoline round %d runs %d\n", round, tramp_runs);
        line("trampoline target", a, b, (10 + round) * N);
    }
}

/* loop(n): for n rounds { cb(); ebx += imm } -> ebx. cb rewrites the immediate in round `at`
 * (inside the call, before the add after it runs). */
static int cb_round, cb_at;
static unsigned char *loop_imm;
static void __cdecl cb(void)
{
    if (++cb_round == cb_at) poke32(loop_imm, 100);
}

static void loop_rewrite(void)
{
    unsigned char *f = buf + 0x3000;
#ifdef _WIN64
    /* loop(n in ecx): rbx, rsi kept, the stack aligned for the call */
    unsigned char c[] = {
        0x53,                               /* push rbx */
        0x56,                               /* push rsi */
        0x48, 0x83, 0xec, 0x28,             /* sub rsp, 40 */
        0x89, 0xce,                         /* mov esi, ecx */
        0x31, 0xdb,                         /* xor ebx, ebx */
        0xe8, 0, 0, 0, 0,                   /* L: call cb */
        0x81, 0xc3, 1, 0, 0, 0,             /* add ebx, imm32 */
        0xff, 0xce,                         /* dec esi */
        0x75, 0xf1,                         /* jnz L */
        0x89, 0xd8,                         /* mov eax, ebx */
        0x48, 0x83, 0xc4, 0x28,             /* add rsp, 40 */
        0x5e,                               /* pop rsi */
        0x5b,                               /* pop rbx */
        0xc3,                               /* ret */
    };
    put(f, c, sizeof c);
    rel32(f + 10, via(f + 0x100, (void *)cb));
    loop_imm = f + 17;
#else
    unsigned char c[] = {
        0x53,                               /* push ebx */
        0x56,                               /* push esi */
        0x8b, 0x74, 0x24, 0x0c,             /* mov esi, [esp+12] */
        0x31, 0xdb,                         /* xor ebx, ebx */
        0xe8, 0, 0, 0, 0,                   /* L: call cb */
        0x81, 0xc3, 1, 0, 0, 0,             /* add ebx, imm32 */
        0x4e,                               /* dec esi */
        0x75, 0xf2,                         /* jnz L */
        0x89, 0xd8,                         /* mov eax, ebx */
        0x5e,                               /* pop esi */
        0x5b,                               /* pop ebx */
        0xc3,                               /* ret */
    };
    put(f, c, sizeof c);
    rel32(f + 8, (void *)cb);
    loop_imm = f + 15;
#endif
    fn g = (fn)f;
    unsigned a = 0;
    cb_at = -1;
    for (int i = 0; i < 300; i++) a += g(1000);
    cb_round = 0;
    cb_at = 400;
    unsigned b = g(1000);
    line("loop rewritten in call", a, b, 399 + 601 * 100);
    cb_round = 0;
    cb_at = -1;
    b = g(1000);
    line("loop after", a, b, 1000 * 100);
}

/* freed, then other code allocated at the same address */
static void reuse(void)
{
    unsigned char *p = VirtualAlloc(NULL, 65536, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE);
    ret_imm(p, 7);
    unsigned a = run((fn)p, N);
    VirtualFree(p, 0, MEM_RELEASE);
    unsigned char *q = VirtualAlloc(p, 65536, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE);
    printf("same address %d\n", q == p);
    if (!q) return;
    ret_imm(q, 8);
    unsigned b = run((fn)q, N);
    line("freed and reused", a, b, 8 * N);
    VirtualFree(q, 0, MEM_RELEASE);
}

/* mov ecx, [esp+4]; mov [next+1], ecx; next: mov eax, imm32; ret: returns its argument
 * (x64: mov [rip+1], ecx; next: ...) */
#ifdef _WIN64
static unsigned char *own_next(unsigned char *f, unsigned k)
{
    unsigned char c[] = { 0x89, 0x0d, 1, 0, 0, 0, 0xb8, 0, 0, 0, 0, 0xc3 };
    memcpy(c + 7, &k, 4);
    put(f, c, sizeof c);
    return f + 7;
}
#else
static unsigned char *own_next(unsigned char *f, unsigned k)
{
    unsigned char c[] = { 0x8b, 0x4c, 0x24, 0x04, 0x89, 0x0d, 0, 0, 0, 0, 0xb8, 0, 0, 0, 0, 0xc3 };
    unsigned char *imm = f + 11;
    memcpy(c + 6, &imm, 4);
    memcpy(c + 11, &k, 4);
    put(f, c, sizeof c);
    return imm;
}
#endif

static void self_rewrite(void)
{
    unsigned char *f = buf + 0x4000;
    own_next(f, 0);
    unsigned a = 0, b = 0;
    for (int i = 0; i < 20000; i++) a += ((fn)f)(i);
    for (int i = 0; i < 20000; i++) b += ((fn)f)(3 * i);
    line("own next instruction", a, b, 3 * a);
}

/* the same, warmed up with one argument (the store leaves the code as it is), then another */
static void self_rewrite_once(void)
{
    unsigned char *f = buf + 0x5000;
    own_next(f, 5);
    unsigned a = 0;
    for (int i = 0; i < 200000; i++) a += ((fn)f)(5);
    unsigned b = ((fn)f)(7), d = ((fn)f)(9);
    printf("%-22s %u %u %u %s\n", "own next, once", a, b, d, a == 5u * 200000 && b == 7 && d == 9 ? "ok" : "STALE");
}

int main(void)
{
    buf = VirtualAlloc(NULL, 65536, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE);
    warm();
    immediates();
    call_sites();
    trampolines();
    loop_rewrite();
    reuse();
    self_rewrite();
    self_rewrite_once();
    return 0;
}
