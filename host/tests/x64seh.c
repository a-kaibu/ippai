#include <windows.h>
#include <stdio.h>
#include <setjmp.h>
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
static LONG CALLBACK veh(EXCEPTION_POINTERS *p) {
    if (p->ExceptionRecord->ExceptionCode == 0xE0000002) return EXCEPTION_CONTINUE_EXECUTION;
    return EXCEPTION_CONTINUE_SEARCH;
}
static void t_veh(void) { RaiseException(0xE0000002, 0, 0, NULL); }
static void t_none(void) { Sleep(0); }
int main(void) {
    AddVectoredExceptionHandler(1, veh);
    struct { const char *n; void (*f)(void); } t[] = {{"none", t_none}, {"seh", t_seh}, {"longjmp", t_jmp}, {"veh", t_veh}};
    for (int i = 0; i < 4; i++) {
        int bad = 0, r = 0;
        for (int k = 0; k < 200; k++) if ((r = check_regs(t[i].f))) { bad++; if (bad == 1) printf("%s: first bad reg %d\n", t[i].n, r); }
        printf("%s: %d/200 bad\n", t[i].n, bad);
    }
    return 0;
}
