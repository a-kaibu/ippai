/* Exceptions as the CPU and kernel32 report them (games and their protectors rely on these):
 * hardware faults (divide by zero, the INT_MIN / -1 overflow, access violations with their
 * read/write/execute parameter, guard and read-only pages, ud2, int3 in both encodings,
 * privileged instructions, single step, INTO/BOUND on x86, unmasked SSE traps), what the
 * CONTEXT holds at each (fault address and IP relative to the faulting function, registers,
 * flags), RaiseException's parameters and limits, and vectored handler / continue handler order.
 * Each fault is a small assembly function; the handler resumes by returning from it. */
#include "../probe.h"

#ifdef _WIN64
#define S(x) #x
#define IP Rip
#define SP Rsp
#define AX Rax
#define CX Rcx
#define DX Rdx
#define PTR unsigned long long
#else
#define S(x) "_" #x
#define IP Eip
#define SP Esp
#define AX Eax
#define CX Ecx
#define DX Edx
#define PTR unsigned long
#endif
#define FN(name, body) \
    void name(void); \
    __asm__(".text\n.globl " S(name) "\n" S(name) ":\n" body "\n")

FN(f_div0, "mov $7, %eax\n xor %ecx, %ecx\n xor %edx, %edx\n div %ecx\n ret");
FN(f_idivov, "mov $0x80000000, %eax\n mov $-1, %ecx\n cltd\n idiv %ecx\n ret");
FN(f_read, "mov $0x10, %ecx\n mov (%ecx), %eax\n ret");
FN(f_write, "mov $0x24, %ecx\n movl $5, (%ecx)\n ret");
FN(f_ud2, "mov $0x11, %eax\n ud2\n ret");
FN(f_int3, "mov $0x22, %eax\n int3\n ret");
FN(f_int3b, "mov $0x33, %eax\n .byte 0xcd, 0x03\n ret");
FN(f_hlt, "hlt\n ret");
FN(f_cli, "cli\n ret");
FN(f_in, "xor %edx, %edx\n in %dx, %al\n ret");
FN(f_int2c, "int $0x2c\n ret");
FN(f_int41, "int $0x41\n ret");
FN(f_movcr, ".byte 0x0f, 0x20, 0xc0\n ret"); /* mov eax/rax, cr0 */
#ifdef _WIN64
FN(f_exec, "mov $0x10, %eax\n call *%rax\n ret");
FN(f_canon, "mov $0x8000000000000000, %rcx\n mov (%rcx), %eax\n ret");
FN(f_step, "pushfq\n orl $0x100, (%rsp)\n popfq\n nop\n nop\n ret");
FN(f_mem, "mov (%rcx), %eax\n ret");
FN(f_memw, "movl $9, (%rcx)\n ret");
FN(f_sse, "mov $0x3f800000, %eax\n movd %eax, %xmm0\n xorps %xmm1, %xmm1\n divss %xmm1, %xmm0\n ret");
FN(f_ssecvt, "mov $0x4f800000, %eax\n movd %eax, %xmm0\n cvttss2si %xmm0, %eax\n ret"); /* masked: no trap */
#else
FN(f_exec, "mov $0x10, %eax\n call *%eax\n ret");
FN(f_step, "pushf\n orl $0x100, (%esp)\n popf\n nop\n nop\n ret");
FN(f_mem, "mov 4(%esp), %ecx\n mov (%ecx), %eax\n ret");
FN(f_memw, "mov 4(%esp), %ecx\n movl $9, (%ecx)\n ret");
FN(f_into, "mov $0x7fffffff, %eax\n add $1, %eax\n into\n ret");
FN(f_bound, "mov 4(%esp), %ecx\n mov $5, %eax\n bound %eax, (%ecx)\n ret");
FN(f_sse, "mov $0x3f800000, %eax\n movd %eax, %xmm0\n xorps %xmm1, %xmm1\n divss %xmm1, %xmm0\n ret");
FN(f_ssecvt, "mov $0x4f800000, %eax\n movd %eax, %xmm0\n cvttss2si %xmm0, %eax\n ret");
#endif

static void *cur; /* the function under test */
static char *page; /* the probe's pages: addresses in them are printed relative to them */
static int resume_at_ip; /* continue where the context points, not by returning */

/* an address: relative to the function under test when inside it, to the pages, or itself */
static void addr(const char *l, void *a) {
    char *p = a;
    if (cur && p >= (char *)cur && p < (char *)cur + 64) printf(" %s f+%ld", l, (long)(p - (char *)cur));
    else if (page && p >= page && p < page + 0x10000) printf(" %s page+%lx", l, (unsigned long)(p - page));
    else printf(" %s %llx", l, (unsigned long long)(ULONG_PTR)p);
}

static void show(const char *who, EXCEPTION_POINTERS *e) {
    EXCEPTION_RECORD *r = e->ExceptionRecord;
    CONTEXT *c = e->ContextRecord;
    printf("  %s code %08lx flags %lx", who, r->ExceptionCode, r->ExceptionFlags);
    if (cur) {
        addr("addr", r->ExceptionAddress);
        addr("ip", (void *)c->IP);
    }
    printf(" n %lu", r->NumberParameters);
    for (DWORD i = 0; i < r->NumberParameters && i < EXCEPTION_MAXIMUM_PARAMETERS; i++) {
        if (i == 1 && (r->ExceptionCode == 0xc0000005 || r->ExceptionCode == 0x80000001)) addr("p1", (void *)r->ExceptionInformation[1]);
        else printf(" p%lu=%llx", i, (unsigned long long)r->ExceptionInformation[i]);
    }
    printf(" ax %llx cx %llx dx %llx tf %d\n", (unsigned long long)(c->AX & 0xffffffff), (unsigned long long)(c->CX & 0xffffffff), (unsigned long long)(c->DX & 0xffffffff), !!(c->EFlags & 0x100));
}

static int calls; /* handler calls in the current test: a runaway stops the probe */
static LONG CALLBACK first(EXCEPTION_POINTERS *e) {
    show("veh", e);
    if (++calls > 4) {
        printf("  runaway\n");
        ExitProcess(3);
    }
    CONTEXT *c = e->ContextRecord;
    DWORD code = e->ExceptionRecord->ExceptionCode;
    if (code == 0xe0000001) return EXCEPTION_CONTINUE_SEARCH; /* for the order test */
    if (code == 0xe0000002) return EXCEPTION_CONTINUE_EXECUTION;
    c->EFlags &= ~0x100;
#ifdef _WIN64
    c->MxCsr = 0x1f80;
#else
    *(DWORD *)(c->ExtendedRegisters + 24) = 0x1f80;
#endif
    if (!resume_at_ip) {
        /* return from the faulting function (its stack is as at entry, or holds the call's
         * return address for f_exec) */
        c->IP = *(PTR *)c->SP;
        c->SP += sizeof(PTR);
    }
    return EXCEPTION_CONTINUE_EXECUTION;
}
static LONG CALLBACK second(EXCEPTION_POINTERS *e) {
    printf("  second %08lx\n", e->ExceptionRecord->ExceptionCode);
    return e->ExceptionRecord->ExceptionCode == 0xe0000001 ? EXCEPTION_CONTINUE_EXECUTION : EXCEPTION_CONTINUE_SEARCH;
}
static LONG CALLBACK cont(EXCEPTION_POINTERS *e) {
    printf("  continue handler %08lx\n", e->ExceptionRecord->ExceptionCode);
    return EXCEPTION_CONTINUE_SEARCH;
}

static void run(const char *name, void (*f)(void)) {
    printf("%s\n", name);
    cur = (void *)f;
    calls = 0;
    f();
    printf("  back\n");
}
static void runp(const char *name, void (*f)(void *), void *arg) {
    printf("%s\n", name);
    cur = (void *)f;
    calls = 0;
    f(arg);
    printf("  back\n");
}

int main(void) {
    void *h1 = AddVectoredExceptionHandler(1, first);
    run("div0", f_div0);
    run("idiv overflow", f_idivov);
    run("read 0x10", f_read);
    run("write 0x24", f_write);
    run("exec 0x10", f_exec);
    run("ud2", f_ud2);
    run("hlt", f_hlt);
    run("cli", f_cli);
    run("in", f_in);
    run("int 2c", f_int2c);
    run("int 41", f_int41);
    run("mov cr0", f_movcr);
#ifdef _WIN64
    run("noncanonical", f_canon);
#else
    run("into", f_into);
    int bounds[2] = { 0, 3 };
    runp("bound", (void (*)(void *))f_bound, bounds);
#endif
    /* single step: resumed where the context points */
    resume_at_ip = 1;
    run("single step", f_step);
    resume_at_ip = 0;
    run("int3", f_int3);
    run("int 3 (cd 03)", f_int3b);

    /* SSE traps once unmasked (the handler masks them again in the context) */
    unsigned int mx = 0x1f80 & ~0x200; /* zero divide unmasked */
    __asm__ volatile("ldmxcsr %0" : : "m"(mx));
    run("sse divide by zero", f_sse);
    mx = 0x1f80 & ~0x81; /* invalid unmasked */
    __asm__ volatile("ldmxcsr %0" : : "m"(mx));
    run("sse invalid cvt", f_ssecvt);
    mx = 0x1f80;
    __asm__ volatile("ldmxcsr %0" : : "m"(mx));
    run("sse masked cvt", f_ssecvt);

    /* pages */
    SYSTEM_INFO si;
    GetSystemInfo(&si);
    char *pg = VirtualAlloc(NULL, si.dwPageSize * 2, MEM_COMMIT, PAGE_READWRITE);
    DWORD old;
    VirtualProtect(pg, si.dwPageSize, PAGE_READWRITE | PAGE_GUARD, &old);
    page = pg;
    struct { const char *n; void (*f)(void *); char *a; DWORD prot; } pt[] = {
        { "guard read", (void (*)(void *))f_mem, pg + 8, 0 },
        { "guard read again", (void (*)(void *))f_mem, pg + 8, 0 },
        { "readonly write", (void (*)(void *))f_memw, pg + si.dwPageSize + 4, PAGE_READONLY },
        { "readonly read", (void (*)(void *))f_mem, pg + si.dwPageSize + 4, 0 },
        { "noaccess read", (void (*)(void *))f_mem, pg + si.dwPageSize + 12, PAGE_NOACCESS },
        { "decommitted write", (void (*)(void *))f_memw, pg + 16, 1 },
    };
    for (unsigned i = 0; i < sizeof pt / sizeof *pt; i++) {
        if (pt[i].prot == 1) VirtualFree(pg, si.dwPageSize, MEM_DECOMMIT);
        else if (pt[i].prot) VirtualProtect(pg + si.dwPageSize, si.dwPageSize, pt[i].prot, &old);
        printf("%s (page+%lx)\n", pt[i].n, (unsigned long)(pt[i].a - pg));
        cur = (void *)pt[i].f;
        calls = 0;
        pt[i].f(pt[i].a);
        printf("  back\n");
    }

    /* RaiseException */
    cur = NULL;
    calls = -100;
    ULONG_PTR args[20];
    for (int i = 0; i < 20; i++) args[i] = 0x100 + i;
    printf("raise 3 params\n");
    RaiseException(0xe0000002, 0, 3, args);
    printf("  back\n");
    printf("raise 20 params\n");
    RaiseException(0xe0000002, 0, 20, args);
    printf("  back\n");
    printf("raise null params\n");
    RaiseException(0xe0000002, 0, 2, NULL);
    printf("  back\n");
    printf("raise flags\n");
    RaiseException(0xe0000002, 0x7ffffffe, 0, NULL); /* flags other than NONCONTINUABLE */
    printf("  back\n");

    /* vectored handler order: the last-added "first" handler runs first; continue handlers
     * run after a handler continues execution */
    void *h2 = AddVectoredExceptionHandler(0, second);
    void *hc = AddVectoredContinueHandler(1, cont);
    printf("order\n");
    RaiseException(0xe0000001, 0, 0, NULL);
    printf("  back\n");
    printf("remove %lu %lu %lu\n", RemoveVectoredExceptionHandler(h2), RemoveVectoredExceptionHandler(h2), RemoveVectoredContinueHandler(hc));
    printf("raise after remove\n");
    RaiseException(0xe0000002, 0, 0, NULL);
    printf("  back\n");
    printf("remove first %lu\n", RemoveVectoredExceptionHandler(h1));
    LPTOP_LEVEL_EXCEPTION_FILTER prev = SetUnhandledExceptionFilter(NULL);
    printf("previous filter set %d\n", prev != NULL);
    return 0;
}
