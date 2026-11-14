/* The SEH chain a thread starts with and single stepping as packers drive it (arch: 32).
 * On Windows every thread runs inside RtlUserThreadStart's __try: fs:[0] at the start routine
 * points at a frame (Next -1, handler in ntdll), never at -1 itself. peC (Malie's malie.exe,
 * #167) installs a frame on that, and its handler unlinks it with `pop fs:[0]` of the saved
 * Next before it returns ExceptionContinueExecution; ExecuteHandler then reloads ESP from
 * fs:[0], so with -1 there ESP became -1. Then: a single-step walk through pushfd/popfd, rep
 * movsb, a call and a jump, the handler setting TF again in the CONTEXT each time, as such
 * decryptors do (where each trap lands, what EFlags the CONTEXT holds, what pushfd stored).
 * arch: 32 */
#include "../probe.h"
#include <intrin.h>

static HMODULE module_of(const void *p) {
    HMODULE m = 0;
    GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, (const char *)p, &m);
    return m;
}

/* the chain from fs:[0]: its last frame, its handler's module, the frames inside the stack */
static void chain(const char *who) {
    NT_TIB *tib = (NT_TIB *)NtCurrentTeb();
    EXCEPTION_REGISTRATION_RECORD *f = tib->ExceptionList, *last = 0;
    int n = 0, inside = 1;
    while (f != (void *)-1 && f && n < 64) {
        if ((char *)f < (char *)tib->StackLimit || (char *)f >= (char *)tib->StackBase) inside = 0;
        last = f;
        f = f->Next;
        n++;
    }
    printf("%s: fs:[0] is -1: %d\n", who, tib->ExceptionList == (void *)-1);
    printf("%s: frames>=1: %d, all inside the stack: %d\n", who, n >= 1, inside);
    if (last) {
        HMODULE m = module_of((void *)last->Handler);
        printf("%s: last handler in ntdll: %d, in the exe: %d\n", who, m && m == GetModuleHandleA("ntdll.dll"), m == GetModuleHandleA(0));
        printf("%s: last frame within 64 KB of the stack base: %d\n", who, (char *)tib->StackBase - (char *)last < 0x10000);
    }
}

/* peC's handler: Next of its own frame back into fs:[0], ESP past the frame, EIP to resume */
static volatile DWORD pec_code, pec_esp_ok;
void pec_resume(void);
static EXCEPTION_DISPOSITION __cdecl pec_handler(EXCEPTION_RECORD *r, void *frame, CONTEXT *c, void *d) {
    DWORD *sp = (DWORD *)c->Esp;
    pec_code = r->ExceptionCode;
    pec_esp_ok = sp == frame;
    __writefsdword(0, sp[0]);
    c->Esp += 8;
    c->Eip = (DWORD)pec_resume;
    return ExceptionContinueExecution;
}
static void pec(const char *who) {
    DWORD before = __readfsdword(0);
    __asm__ volatile(
        "pushl %0\n pushl %%fs:0\n movl %%esp, %%fs:0\n"
        "xorl %%eax, %%eax\n movl (%%eax), %%eax\n"
        ".globl _pec_resume\n_pec_resume:\n"
        : : "r"(pec_handler) : "eax", "memory");
    printf("%s: peC frame: code %08lx, frame at ESP %lu, fs:[0] back %d\n", who, pec_code, pec_esp_ok, __readfsdword(0) == before);
}

/* single stepping: the walked function, its trap addresses as offsets from its start */
void f_walk(void);
void f_walk_end(void);
void f_walk_far(void);
void f_walk_popped(void);
__asm__(".text\n.globl _f_walk\n_f_walk:\n"
        "pushl %esi\n pushl %edi\n pushl %ecx\n"
        "pushfl\n orl $0x100, (%esp)\n popfl\n" /* TF on: the trap after the next instruction */
        "nop\n"
        "movl %edx, %esi\n leal 8(%edx), %edi\n movl $3, %ecx\n cld\n rep movsb\n"
        "pushfl\n popl %eax\n" /* what pushfd saw */
        "movl %eax, 16(%edx)\n"
        "xorl %ecx, %ecx\n rep movsb\n" /* ECX 0: no iteration */
        "call 1f\n jmp 2f\n1: ret\n"
        "2: jmp _f_walk_far\n"
        ".globl _f_walk_far\n_f_walk_far:\n"
        "pushfl\n andl $0xfffffeff, (%esp)\n popfl\n" /* TF off: popfd itself still traps */
        ".globl _f_walk_popped\n_f_walk_popped:\n"
        "nop\n"
        ".globl _f_walk_end\n_f_walk_end:\n"
        "popl %ecx\n popl %edi\n popl %esi\n ret\n");

static volatile int steps;
static DWORD at[64], fl[64];
static LONG CALLBACK step_veh(EXCEPTION_POINTERS *e) {
    if (e->ExceptionRecord->ExceptionCode != EXCEPTION_SINGLE_STEP) return EXCEPTION_CONTINUE_SEARCH;
    CONTEXT *c = e->ContextRecord;
    if (steps < 64) {
        at[steps] = c->Eip - (DWORD)f_walk;
        fl[steps] = c->EFlags;
    }
    steps++;
    /* again until the popfd that turns TF off (which traps itself) */
    if (c->Eip < (DWORD)f_walk_popped) c->EFlags |= 0x100;
    return EXCEPTION_CONTINUE_EXECUTION;
}
static void walk(void) {
    static unsigned char buf[32] = "abcdefgh";
    void *v = AddVectoredExceptionHandler(1, step_veh);
    __asm__ volatile("call _f_walk" : : "d"(buf) : "eax", "memory", "cc");
    RemoveVectoredExceptionHandler(v);
    printf("steps %d\n", steps);
    for (int i = 0; i < steps && i < 64; i++)
        printf("step %d at +%lu tf %lu rf %lu\n", i, at[i], (fl[i] >> 8) & 1, (fl[i] >> 16) & 1);
    printf("copied %.3s, pushfd saw TF %lu\n", buf + 8, (*(DWORD *)(buf + 16) >> 8) & 1);
}

static DWORD WINAPI thread(void *p) {
    chain("thread");
    pec("thread");
    return 0;
}

int main(void) {
    chain("main");
    HANDLE t = CreateThread(0, 0, thread, 0, 0, 0);
    WaitForSingleObject(t, INFINITE);
    CloseHandle(t);
    walk();
    return 0;
}
