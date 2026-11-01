/* JIT check for vnvm: a hot loop calls through a function pointer that ends up null. The call
 * must fault at address 0 (as on Windows), not run some other translated code: 0 is the key of
 * an empty slot in the JIT's block table. Prints "nullcall: ok" when the fault is at 0.
 * Build: i686-w64-mingw32-gcc -O2 nullcall.c -o nullcall.exe */
#include <windows.h>
#include <stdio.h>

static volatile int sum;
static void add(int x) { sum += x; }
static void (*volatile fn)(int) = add;

static LONG CALLBACK handler(EXCEPTION_POINTERS *e)
{
    void *at = e->ExceptionRecord->ExceptionAddress;
    printf("nullcall: %s (code %#lx at %p)\n", at == NULL ? "ok" : "FAIL", e->ExceptionRecord->ExceptionCode, at);
    fflush(stdout);
    ExitProcess(at == NULL ? 0 : 1);
}

int main(void)
{
    AddVectoredExceptionHandler(1, handler);
    for (int i = 0; i < 200000; i++)
    {
        if (i == 199999) fn = NULL;
        fn(i);
    }
    printf("nullcall: FAIL (no fault, sum %d)\n", sum);
    return 1;
}
