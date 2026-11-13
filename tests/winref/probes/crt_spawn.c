/* The C runtime's _spawnv / _execv family, as launchers use it (Ren'Py's AAA_0.99.exe starts
 * lib\windows-i686\AAA_0.99.exe with _wexecv and says "Could not execute" when it fails): the
 * probe starts itself as a child. _P_WAIT gives the child's exit code, _P_NOWAIT a process handle
 * that _cwait waits on, the arguments reach the child joined by single spaces as they are (no
 * quotes added: "a b" arrives as two arguments), a missing program is -1 / ENOENT, a bad mode
 * EINVAL, and _execv ends this process once the child started (the child's line comes last). */
#include "../probe.h"
#include <process.h>
#include <errno.h>

int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "child")) {
        printf("child argc %d", argc);
        for (int i = 2; i < argc; i++) printf(" [%s]", argv[i]);
        printf("\n");
        fflush(stdout);
        return argc > 2 ? atoi(argv[2]) : 0;
    }
    char self[MAX_PATH];
    GetModuleFileNameA(NULL, self, MAX_PATH);
    fflush(stdout);

    const char *a1[] = { self, "child", "7", NULL };
    errno = 0;
    intptr_t r = _spawnv(_P_WAIT, self, a1);
    printf("spawn wait %d errno %d\n", (int)r, errno);
    fflush(stdout);

    const char *a2[] = { self, "child", "3", "a b", NULL };
    intptr_t h = _spawnv(_P_NOWAIT, self, a2);
    int stat = -1;
    intptr_t w = _cwait(&stat, h, 0);
    printf("nowait handle ok %d cwait same %d stat %d\n", h > 0, w == h, stat);
    fflush(stdout);

    const wchar_t *a3[] = { L"x", L"child", L"5", NULL };
    wchar_t wself[MAX_PATH];
    GetModuleFileNameW(NULL, wself, MAX_PATH);
    printf("wspawn wait %d\n", (int)_wspawnv(_P_WAIT, wself, a3));
    fflush(stdout);

    const char *a4[] = { "nope", NULL };
    errno = 0;
    r = _spawnv(_P_WAIT, "no_such_program_here.exe", a4);
    printf("missing %d errno %d\n", (int)r, errno);
    fflush(stdout);

    const char *a5[] = { self, "child", "0", "last", NULL };
    _execv(self, a5);
    printf("exec returned errno %d\n", errno);
    return 1;
}
