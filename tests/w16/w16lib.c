/* 16-bit DLL of dll16.c: exported functions (by ordinal and by name), its own data segment
 * (a counter, a string, what LibMain was given), a callback into the program. */
#include <windows.h>

static int calls;
static WORD init_ds, init_heap;
static HINSTANCE init_inst;
static char hello[] = "from the DLL's data";

int FAR PASCAL __export LibMain(HINSTANCE inst, WORD ds, WORD heap, LPSTR cmd) {
    init_inst = inst;
    init_ds = ds;
    init_heap = heap;
    calls = 100;
    return 1;
}

int FAR PASCAL __export WEP(int how) {
    return 1;
}

int FAR PASCAL __export LibAdd(int a, int b) {
    return a + b;
}

int FAR PASCAL __export LibCount(void) {
    return ++calls;
}

LPSTR FAR PASCAL __export LibHello(void) {
    return hello;
}

long FAR PASCAL __export LibCallBack(long (FAR PASCAL *f)(int), int v) {
    return f(v) + 1;
}

int FAR PASCAL __export LibSameDs(void) {
    return init_ds == (WORD)init_inst;
}
