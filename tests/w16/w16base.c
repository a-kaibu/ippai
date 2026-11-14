/* 16-bit DLL that w16lib.c imports (a DLL's own imports from another DLL), with its own data. */
#include <windows.h>

static int base = 1000;

int FAR PASCAL __export LibMain(HINSTANCE inst, WORD ds, WORD heap, LPSTR cmd) {
    base += 1;
    return 1;
}

int FAR PASCAL __export WEP(int how) {
    return 1;
}

int FAR PASCAL __export BaseMul(int a, int b) {
    return a * b + base;
}
