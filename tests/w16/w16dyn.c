/* 16-bit DLL of dll16.c that only LoadLibrary loads: its LibMain sees fresh data at each load. */
#include <windows.h>

static int loads;

int FAR PASCAL __export LibMain(HINSTANCE inst, WORD ds, WORD heap, LPSTR cmd) {
    loads++;
    return 1;
}

int FAR PASCAL __export WEP(int how) {
    return 1;
}

int FAR PASCAL __export DynLoads(void) {
    return loads;
}
