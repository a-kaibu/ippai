/* Save states (docs/savestate.md): a state asked for while the program is still starting (its
 * TLS callback runs, called by sake before the entry point, and sleeps) is taken only once the
 * program runs on its own: a restored copy has no host left to return to from the callback.
 * tests/snap.sh cuts it in the callback's sleep. Build: i686-w64-mingw32-gcc -O2 snapinit.c */
#include <stdio.h>
#include <windows.h>

static void NTAPI callback(void *module, DWORD reason, void *reserved)
{
    if (reason != DLL_PROCESS_ATTACH) return;
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("callback\n");
    Sleep(2500);
    printf("callback done\n");
}
#ifdef _WIN64
#pragma comment(linker, "/INCLUDE:_tls_used")
#endif
__attribute__((section(".CRT$XLB"), used)) PIMAGE_TLS_CALLBACK snapinit_callback = callback;

int main(void)
{
    for (int k = 0; k < 10; k++)
    {
        printf("step %d\n", k);
        Sleep(100);
    }
    printf("end\n");
    return 0;
}
