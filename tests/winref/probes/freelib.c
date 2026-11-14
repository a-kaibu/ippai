/* LoadLibrary / FreeLibrary counting references, DllMain's PROCESS_ATTACH and PROCESS_DETACH, and
 * a DLL's imports holding the DLLs they import (Wine's msvcp140 test and plugin loaders reload
 * DLLs). The same source builds the probe and two DLLs beside it (build.sh, "dlls: lib dep"):
 * freelib-dep-NN.dll, and freelib-lib-NN.dll importing from it. sake never unloaded a DLL (#58).
 * dlls: lib dep */
#include "../probe.h"

#ifdef PROBE_DEP
static int count;
__declspec(dllexport) int dep_next(void) { return ++count; }
BOOL WINAPI DllMain(HINSTANCE h, DWORD why, LPVOID reserved) {
    if (why == DLL_PROCESS_ATTACH || why == DLL_PROCESS_DETACH) {
        printf("  dep %s%s\n", why ? "attach" : "detach", why == DLL_PROCESS_DETACH && reserved ? " (process end)" : "");
        fflush(stdout);
    }
    return TRUE;
}
#elif defined PROBE_LIB
__declspec(dllimport) int dep_next(void);
static int count;
__declspec(dllexport) int lib_next(void) { return ++count * 100 + dep_next(); }
BOOL WINAPI DllMain(HINSTANCE h, DWORD why, LPVOID reserved) {
    if (why == DLL_PROCESS_ATTACH || why == DLL_PROCESS_DETACH) {
        printf("  lib %s%s\n", why ? "attach" : "detach", why == DLL_PROCESS_DETACH && reserved ? " (process end)" : "");
        fflush(stdout);
    }
    return TRUE;
}
#else
#define LIB (sizeof(void *) == 8 ? "freelib-lib-64.dll" : "freelib-lib-32.dll")
#define DEP (sizeof(void *) == 8 ? "freelib-dep-64.dll" : "freelib-dep-32.dll")

static void loaded(void) {
    printf("  loaded: lib %d dep %d\n", GetModuleHandleA(LIB) != NULL, GetModuleHandleA(DEP) != NULL);
}
static int next(HMODULE h) {
    int (*f)(void) = (int (*)(void))GetProcAddress(h, "lib_next");
    return f ? f() : -1;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("LoadLibrary lib\n");
    HMODULE h = LoadLibraryA(LIB);
    printf("  got it %d\n", h != NULL);
    if (!h) return 0;
    loaded();
    printf("  lib_next %d %d\n", next(h), next(h));
    printf("LoadLibrary lib again: the same %d\n", LoadLibraryA(LIB) == h);
    printf("FreeLibrary lib %d\n", FreeLibrary(h));
    loaded();
    printf("LoadLibrary dep\n");
    HMODULE d = LoadLibraryA(DEP);
    printf("FreeLibrary lib %d\n", FreeLibrary(h));
    loaded();
    printf("FreeLibrary dep %d\n", FreeLibrary(d));
    loaded();
    SetLastError(0xdead);
    BOOL r = FreeLibrary(d);
    printf("FreeLibrary dep again %d error %lu\n", r, r ? 0 : GetLastError());

    printf("LoadLibrary lib, a second time\n");
    h = LoadLibraryA(LIB);
    printf("  lib_next from the start %d\n", next(h));
    printf("GetModuleHandleEx dep (a reference) %d\n", GetModuleHandleExA(0, DEP, &d));
    printf("FreeLibrary lib %d\n", FreeLibrary(h));
    loaded();
    printf("FreeLibrary dep %d\n", FreeLibrary(d));
    loaded();

    printf("LoadLibrary dep, pinned\n");
    d = LoadLibraryA(DEP);
    HMODULE p;
    printf("  pin %d\n", GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_PIN, DEP, &p));
    printf("FreeLibrary dep %d\n", FreeLibrary(d));
    loaded();
    printf("FreeLibrary dep again %d\n", FreeLibrary(d));
    loaded();
    printf("end\n");
    return 0;
}
#endif
