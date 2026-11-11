/* _beginthread / _beginthreadex: no start routine is EINVAL with no thread (0 / -1), as
 * Windows' CRT answers (Wine's ucrtbase:thread: sake started a thread at address 0, which
 * crashed the test now and then); a real one runs and hands back its value.
 * Build: i686-w64-mingw32-gcc -O2 beginthread.c -o beginthread.exe (also x86-64) */
#include <errno.h>
#include <process.h>
#include <stdio.h>
#include <stdlib.h>
#include <windows.h>

static void __cdecl quiet(const wchar_t *e, const wchar_t *f, const wchar_t *file, unsigned line, uintptr_t r)
{
}

static unsigned __stdcall twice(void *arg)
{
    return 2 * (unsigned)(uintptr_t)arg;
}

static volatile LONG ran;
static void __cdecl mark(void *arg)
{
    InterlockedExchange(&ran, (LONG)(uintptr_t)arg);
}

int main(void)
{
    uintptr_t h;
    DWORD code = 0;
    int e;

    _set_invalid_parameter_handler(quiet);
    errno = 0;
    h = _beginthreadex(NULL, 0, NULL, NULL, 0, NULL);
    e = errno;
    printf("beginthreadex null: %d errno %d\n", (int)h, e);
    errno = 0;
    h = _beginthread(NULL, 0, NULL);
    e = errno;
    printf("beginthread null: %d errno %d\n", (int)(intptr_t)h, e);
    /* (nothing ran at address 0 meanwhile: the process is still here) */
    Sleep(50);
    h = _beginthreadex(NULL, 0, twice, (void *)21, 0, NULL);
    WaitForSingleObject((HANDLE)h, INFINITE);
    GetExitCodeThread((HANDLE)h, &code);
    CloseHandle((HANDLE)h);
    printf("beginthreadex: %lu\n", code);
    h = _beginthread(mark, 0, (void *)7);
    for (int i = 0; i < 500 && !ran; i++)
        Sleep(10);
    printf("beginthread: %ld\n", ran);
    return 0;
}
