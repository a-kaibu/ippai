/* Save states (docs/savestate.md): a program that starts another process can't be saved (the
 * kernel the two share is outside the memory): the state fails and says so, the program goes on
 * as if none was asked for. tests/snap.sh. Build: i686-w64-mingw32-gcc -O2 snapproc.c (and
 * -DCHILD for snapchild.exe, the process it starts, next to it). */
#include <stdio.h>
#include <string.h>
#include <windows.h>

int main(void)
{
#ifdef CHILD
    Sleep(3000);
    return 7;
#else
    setvbuf(stdout, NULL, _IONBF, 0);
    char path[MAX_PATH];
    GetModuleFileNameA(NULL, path, MAX_PATH);
    strcpy(strrchr(path, '\\') + 1, "snapchild.exe");
    STARTUPINFOA si = {sizeof si};
    PROCESS_INFORMATION pi;
    if (!CreateProcessA(path, NULL, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi))
    {
        printf("CreateProcess: %lu\n", GetLastError());
        return 1;
    }
    printf("started\n");
    WaitForSingleObject(pi.hProcess, INFINITE);
    DWORD code = 0;
    GetExitCodeProcess(pi.hProcess, &code);
    printf("child: %lu\nend\n", code);
    return 0;
#endif
}
