#include <windows.h>
#include <stdio.h>
#include <setjmp.h>
#include <intrin.h>
static volatile int depth_hits;
__declspec(noinline) static void deep(int n) {
    volatile char buf[48]; buf[0] = n;
    if (n == 0) RaiseException(0xE0000001, 0, 0, NULL);
    deep(n - 1); depth_hits += buf[0];
}
static jmp_buf jb;
__declspec(noinline) static void deepj(int n) {
    volatile char buf[48]; buf[0] = n;
    if (n == 0) longjmp(jb, 1);
    deepj(n - 1); depth_hits += buf[0];
}
/* runs f with callee-saved regs set to known values, checks them after f returns */
extern int check_regs(void (*f)(void));
__asm__(
".globl check_regs\n"
"check_regs:\n"
" push %rbx; push %rbp; push %rsi; push %rdi; push %r12; push %r13; push %r14; push %r15\n"
" sub $0xa8,%rsp\n"
" movdqu %xmm6,0x00(%rsp); movdqu %xmm7,0x10(%rsp); movdqu %xmm8,0x20(%rsp); movdqu %xmm9,0x30(%rsp); movdqu %xmm10,0x40(%rsp)\n"
" movdqu %xmm11,0x50(%rsp); movdqu %xmm12,0x60(%rsp); movdqu %xmm13,0x70(%rsp); movdqu %xmm14,0x80(%rsp); movdqu %xmm15,0x90(%rsp)\n"
" mov $0x1111111111111111,%rbx; mov $0x2222222222222222,%rbp; mov $0x3333333333333333,%rsi; mov $0x4444444444444444,%rdi\n"
" mov $0x5555555555555555,%r12; mov $0x6666666666666666,%r13; mov $0x7777777777777777,%r14; mov $0x8888888888888888,%r15\n"
" mov $0x0606060606060606,%rax; movq %rax,%xmm6; pshufd $0x44,%xmm6,%xmm6\n"
" mov $0x0f0f0f0f0f0f0f0f,%rax; movq %rax,%xmm15; pshufd $0x44,%xmm15,%xmm15\n"
" mov $0x0a0a0a0a0a0a0a0a,%rax; movq %rax,%xmm10; pshufd $0x44,%xmm10,%xmm10\n"
" sub $0x28,%rsp; call *%rcx; add $0x28,%rsp\n"
" xor %eax,%eax\n"
" mov $0x1111111111111111,%rcx; cmp %rcx,%rbx; jne 1f\n"
" mov $0x2222222222222222,%rcx; cmp %rcx,%rbp; jne 2f\n"
" mov $0x3333333333333333,%rcx; cmp %rcx,%rsi; jne 3f\n"
" mov $0x4444444444444444,%rcx; cmp %rcx,%rdi; jne 4f\n"
" mov $0x5555555555555555,%rcx; cmp %rcx,%r12; jne 5f\n"
" mov $0x6666666666666666,%rcx; cmp %rcx,%r13; jne 6f\n"
" mov $0x7777777777777777,%rcx; cmp %rcx,%r14; jne 7f\n"
" mov $0x8888888888888888,%rcx; cmp %rcx,%r15; jne 8f\n"
" movq %xmm6,%rcx; mov $0x0606060606060606,%rdx; cmp %rdx,%rcx; jne 9f\n"
" movq %xmm15,%rcx; mov $0x0f0f0f0f0f0f0f0f,%rdx; cmp %rdx,%rcx; jne 10f\n"
" movq %xmm10,%rcx; mov $0x0a0a0a0a0a0a0a0a,%rdx; cmp %rdx,%rcx; jne 11f\n"
" jmp 99f\n"
"1: mov $1,%eax; jmp 99f\n2: mov $2,%eax; jmp 99f\n3: mov $3,%eax; jmp 99f\n4: mov $4,%eax; jmp 99f\n"
"5: mov $5,%eax; jmp 99f\n6: mov $6,%eax; jmp 99f\n7: mov $7,%eax; jmp 99f\n8: mov $8,%eax; jmp 99f\n"
"9: mov $9,%eax; jmp 99f\n10: mov $10,%eax; jmp 99f\n11: mov $11,%eax\n"
"99:\n"
" movdqu 0x00(%rsp),%xmm6; movdqu 0x10(%rsp),%xmm7; movdqu 0x20(%rsp),%xmm8; movdqu 0x30(%rsp),%xmm9; movdqu 0x40(%rsp),%xmm10\n"
" movdqu 0x50(%rsp),%xmm11; movdqu 0x60(%rsp),%xmm12; movdqu 0x70(%rsp),%xmm13; movdqu 0x80(%rsp),%xmm14; movdqu 0x90(%rsp),%xmm15\n"
" add $0xa8,%rsp\n"
" pop %r15; pop %r14; pop %r13; pop %r12; pop %rdi; pop %rsi; pop %rbp; pop %rbx; ret\n");

static void t_seh(void) {
    __try { deep(5); } __except (EXCEPTION_EXECUTE_HANDLER) { depth_hits++; }
}
static void t_jmp(void) {
    if (!setjmp(jb)) deepj(5);
}
/* where an exception none of the tests raises came from: code, the module and offset of the
   address, the first parameters (issue #96: the longjmp test ends the process on Windows) */
static void where(const char *what, EXCEPTION_POINTERS *p) {
    EXCEPTION_RECORD *r = p->ExceptionRecord;
    HMODULE m = NULL;
    char name[MAX_PATH] = "?";
    if (GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, (LPCSTR)r->ExceptionAddress, &m))
        GetModuleFileNameA(m, name, sizeof name);
    const char *base = strrchr(name, '\\') ? strrchr(name, '\\') + 1 : name;
    printf("%s: %08lx at %s+%llx, parameters %llx %llx\n", what, r->ExceptionCode, base, (ULONG64)r->ExceptionAddress - (ULONG64)m,
        r->NumberParameters > 0 ? (ULONG64)r->ExceptionInformation[0] : 0, r->NumberParameters > 1 ? (ULONG64)r->ExceptionInformation[1] : 0);
}
static LONG CALLBACK veh(EXCEPTION_POINTERS *p) {
    DWORD code = p->ExceptionRecord->ExceptionCode;
    if (code == 0xE0000002) return EXCEPTION_CONTINUE_EXECUTION;
    if (code != 0xE0000001 && code != 0xE0000003) where("exception", p);
    return EXCEPTION_CONTINUE_SEARCH;
}
static LONG WINAPI unhandled(EXCEPTION_POINTERS *p) {
    where("unhandled", p);
    return EXCEPTION_EXECUTE_HANDLER;
}
static void t_veh(void) { RaiseException(0xE0000002, 0, 0, NULL); }
static void t_none(void) { Sleep(0); }

/* RtlUnwindEx with a STATUS_UNWIND_CONSOLIDATE record (as the C++ runtimes run catch blocks):
   the callback writes its home space, as MSVC's __CxxCallCatchBlock keeps registers there, and
   returns where the target frame goes on; or it raises an exception, which the frames above the
   target catch (a throw from a catch block) */
static volatile int cons_raise, cons_where;
extern void *cons_cb(EXCEPTION_RECORD *rec);
__declspec(noinline) void *cons_body(EXCEPTION_RECORD *rec) {
    if (cons_raise) RaiseException(0xE0000003, 0, 0, NULL);
    return (void *)rec->ExceptionInformation[1];
}
__asm__(
".globl cons_cb\n"
".def cons_cb; .scl 2; .type 32; .endef\n"
".seh_proc cons_cb\n"
"cons_cb:\n"
" mov %rcx,8(%rsp); movq $-1,16(%rsp); movq $-1,24(%rsp); movq $-1,32(%rsp)\n"
" sub $0x28,%rsp\n"
" .seh_stackalloc 0x28\n"
" .seh_endprologue\n"
" call cons_body\n"
" add $0x28,%rsp\n"
" ret\n"
" .seh_endproc\n");
__declspec(noinline) static ULONG64 caller_sp(void) { return (ULONG64)_AddressOfReturnAddress() + 8; }
__declspec(noinline) static int cons_target(void) {
    volatile int where = 1;
    EXCEPTION_RECORD rec = {0};
    CONTEXT ctx;
    rec.ExceptionCode = STATUS_UNWIND_CONSOLIDATE;
    rec.NumberParameters = 2;
    rec.ExceptionInformation[0] = (ULONG_PTR)cons_cb;
    rec.ExceptionInformation[1] = (ULONG_PTR)&&resume;
    if (where == 1) {
        where = 2;
        RtlUnwindEx((void *)caller_sp(), &&resume, &rec, NULL, &ctx, NULL);
        return -1;
    }
resume:
    return where;
}
static void t_cons(void) {
    cons_raise = 0;
    cons_where = cons_target();
}
static void t_cons_raise(void) {
    cons_raise = 1;
    cons_where = 0;
    __try { cons_where = cons_target(); } __except (GetExceptionCode() == 0xE0000003) { cons_where = 3; }
}
int main(void) {
    /* (stdout unbuffered, and each test named before it runs: the probe crashed on Windows with
       no line out, issue #96) */
    setvbuf(stdout, NULL, _IONBF, 0);
    AddVectoredExceptionHandler(1, veh);
    SetUnhandledExceptionFilter(unhandled);
    struct { const char *n; void (*f)(void); } t[] = {{"none", t_none}, {"seh", t_seh}, {"longjmp", t_jmp}, {"veh", t_veh}, {"consolidate", t_cons}, {"consolidate raise", t_cons_raise}};
    for (int i = 0; i < 6; i++) {
        int bad = 0, r = 0;
        printf("start %s\n", t[i].n);
        for (int k = 0; k < 200; k++) if ((r = check_regs(t[i].f))) { bad++; if (bad == 1) printf("%s: first bad reg %d\n", t[i].n, r); }
        printf("%s: %d/200 bad", t[i].n, bad);
        if (i >= 4) printf(", went on at %d", cons_where);
        printf("\n");
    }
    return 0;
}
