/* Crash-log check for vnvm (crates/vnhost/src/crashlog.rs, web/src/crashlog.ts): makes a few
 * syscalls, then crashes the way games do, with no handler, so Wine reports an unhandled
 * exception and the page saves a crash report. The report should show the faulting eip in
 * crashtest.exe, the frames through crash_deep/crash_mid, the CreateFileW/ReadFile/Sleep
 * calls just before, and the module list.
 *   crashtest.exe          write through a null pointer
 *   crashtest.exe ud       illegal instruction
 *   crashtest.exe div      integer divide by zero
 *   crashtest.exe throw    RaiseException(0xe0001234) (a C++-style throw nobody catches)
 *   crashtest.exe handled  a fault its own handler catches, then a normal exit (no crash report)
 * Build: i686-w64-mingw32-gcc -O1 -fno-omit-frame-pointer crashtest.c -o crashtest.exe
 *        (x86_64-w64-mingw32-gcc for the 64-bit one) */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static volatile int zero;

__attribute__((noinline)) static void crash_deep(const char *how)
{
    if (!strcmp(how, "ud")) __asm__ volatile("ud2");
    if (!strcmp(how, "div")) printf("%d\n", 100 / zero);
    if (!strcmp(how, "throw")) RaiseException(0xe0001234, 0, 0, NULL);
    *(volatile int *)(ULONG_PTR)(zero + 0x10) = 0x1234;
}

__attribute__((noinline)) static void crash_mid(const char *how)
{
    crash_deep(how);
    printf("crashtest: still alive?\n");
}

static LONG CALLBACK handler(EXCEPTION_POINTERS *e)
{
    printf("crashtest: handled %#lx at %p\n", e->ExceptionRecord->ExceptionCode, e->ExceptionRecord->ExceptionAddress);
    fflush(stdout);
    ExitProcess(0);
}

int main(int argc, char **argv)
{
    const char *how = argc > 1 ? argv[1] : "null";
    char buf[16];
    DWORD got = 0;
    HANDLE f = CreateFileW(L"C:\\windows\\win.ini", GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    if (f != INVALID_HANDLE_VALUE)
    {
        ReadFile(f, buf, sizeof(buf), &got, NULL);
        CloseHandle(f);
    }
    Sleep(10);
    printf("crashtest: %s (read %lu bytes)\n", how, got);
    fflush(stdout);
    if (!strcmp(how, "handled")) AddVectoredExceptionHandler(1, handler);
    crash_mid(how);
    return 0;
}
