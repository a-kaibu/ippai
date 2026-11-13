/* libs: -Wl,--stack,0x400000 */
/* Threads' stacks against the image's SizeOfStackReserve (4 MB here): the main thread and a
 * CreateThread that asks for less get the image's reserve, a commit past it is rounded up to
 * 1 MB, STACK_SIZE_PARAM_IS_A_RESERVATION is the reserve. 交姦条件 (Delphi, 4 MB) has a 1 MB
 * frame its sound thread probes page by page; with sake's old flat 1 MB it overflowed. */
#include <windows.h>
#include <stdio.h>

/* Reserve of the calling thread's stack, in KB: its top (TEB StackBase) to its allocation base */
static unsigned reserve_kb(void) {
    MEMORY_BASIC_INFORMATION mi;
    volatile char here;
    NT_TIB *tib = (NT_TIB *)NtCurrentTeb();
    if (!VirtualQuery((void *)&here, &mi, sizeof mi)) return 0;
    return (unsigned)(((char *)tib->StackBase - (char *)mi.AllocationBase) / 1024);
}

/* A frame of `kb` KB, probed by ___chkstk_ms as the compiler does, then touched at the bottom */
static __attribute__((noinline)) int deep(unsigned kb) {
    volatile char *b = __builtin_alloca(kb * 1024);
    b[0] = 1;
    b[kb * 1024 - 1] = 2;
    return b[0] + b[kb * 1024 - 1];
}

static DWORD WINAPI thread(void *p) {
    unsigned use = (unsigned)(UINT_PTR)p;
    printf("  reserve %u KB", reserve_kb());
    if (use) printf(", %u KB frame: %d", use, deep(use));
    printf("\n");
    fflush(stdout);
    return 0;
}

static void run(const char *label, SIZE_T size, DWORD flags, unsigned use) {
    printf("%s:", label);
    fflush(stdout);
    HANDLE h = CreateThread(NULL, size, thread, (void *)(UINT_PTR)use, flags, NULL);
    if (!h) {
        printf(" CreateThread %lu\n", GetLastError());
        return;
    }
    WaitForSingleObject(h, INFINITE);
    CloseHandle(h);
}

int main(void) {
    printf("main: reserve %u KB, 3072 KB frame: %d\n", reserve_kb(), deep(3072));
    run("default", 0, 0, 3072);
    run("64 KB commit", 0x10000, 0, 3072);
    run("5 MB + 1 commit", 5 * 1048576 + 1, 0, 5120);
    run("8 MB reserve", 8 * 1048576, STACK_SIZE_PARAM_IS_A_RESERVATION, 7168);
    run("2 MB reserve", 2 * 1048576, STACK_SIZE_PARAM_IS_A_RESERVATION, 1536);
    return 0;
}
