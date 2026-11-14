/* A 16-bit DLL beside the program (w16lib.c), as games ship them: imported by ordinal and by
 * name at load, its LibMain run first with its own data segment, its exports called (arguments,
 * its data, a far string in it, a callback into the program), then LoadLibrary /
 * GetModuleHandle / GetProcAddress / FreeLibrary on it. sake loaded no 16-bit DLL (#37).
 * Also a DLL importing from another DLL (w16base), and one only LoadLibrary loads (w16dyn): by
 * ordinal, unloaded at its last FreeLibrary, loaded afresh.
 * dll: w16base w16lib w16dyn
 * libs: w16lib.lib */
#include <windows.h>
#include <stdio.h>

int FAR PASCAL LibAdd(int a, int b);
int FAR PASCAL LibCount(void);
LPSTR FAR PASCAL LibHello(void);
long FAR PASCAL LibCallBack(long (FAR PASCAL *f)(int), int v);
int FAR PASCAL LibSameDs(void);
int FAR PASCAL LibMulAdd(int a, int b, int c);

static long FAR PASCAL twice(int v) {
    return 2L * v;
}

int PASCAL WinMain(HINSTANCE hi, HINSTANCE hp, LPSTR cmd, int sh) {
    HINSTANCE h, h2;
    FARPROC f;
    int n;
    printf("LibAdd(2, 40) %d\n", LibAdd(2, 40));
    n = LibCount();
    printf("LibCount after LibMain %d", n);
    printf(" %d\n", LibCount());
    printf("LibHello \"%s\"\n", (LPSTR)LibHello());
    printf("LibCallBack %ld\n", LibCallBack(twice, 20));
    printf("LibMain had its instance as its data %d\n", LibSameDs());
    h = GetModuleHandle("W16LIB");
    printf("GetModuleHandle %s\n", h ? "found" : "none");
    h2 = LoadLibrary("W16LIB.DLL");
    printf("LoadLibrary %s\n", (UINT)h2 >= 32 ? "ok" : "failed");
    f = GetProcAddress(h2, "LIBADD");
    printf("GetProcAddress LIBADD %s", f ? "found" : "none");
    if (f) printf(" -> %d", ((int (FAR PASCAL *)(int, int))f)(5, 6));
    printf("\n");
    printf("GetProcAddress missing %s\n", GetProcAddress(h2, "NOSUCH") ? "found" : "none");
    FreeLibrary(h2);
    printf("still loaded after one FreeLibrary %d\n", GetModuleHandle("W16LIB") != 0);
    printf("LoadLibrary missing %u\n", (unsigned)LoadLibrary("NOSUCH.DLL"));
    printf("LibMulAdd through w16base %d\n", LibMulAdd(6, 7, 5));
    for (n = 0; n < 2; n++) {
        h = LoadLibrary("w16dyn");
        f = GetProcAddress(h, "DynLoads");
        printf("w16dyn %s, loads %d", (UINT)h >= 32 ? "loaded" : "failed", f ? ((int (FAR PASCAL *)(void))f)() : -1);
        printf(", by ordinal the same %d\n", GetProcAddress(h, (LPCSTR)MAKELONG(3, 0)) == f);
        h2 = LoadLibrary("W16DYN.DLL");
        printf("again the same %d\n", h2 == h);
        FreeLibrary(h2);
        printf("one FreeLibrary: loaded %d\n", GetModuleHandle("W16DYN") != 0);
        FreeLibrary(h);
        printf("the last FreeLibrary: loaded %d\n", GetModuleHandle("W16DYN") != 0);
    }
    return 0;
}
