/* Four threads print numbered lines with the time while the page's own thread is kept busy
   (tests/output-browser.mjs): the lines all arrive, whole and in each thread's order, and a
   thread's writes do not wait for the page's thread (#221). One WriteFile a line: mingw's printf
   writes a line in pieces. */
#include <windows.h>

#define LINES 400

static DWORD WINAPI run(void *p) {
    int t = (int)(INT_PTR)p;
    HANDLE out = GetStdHandle(STD_OUTPUT_HANDLE);
    char b[64];
    DWORD w;
    for (int i = 0; i < LINES; i++) {
        int n = wsprintfA(b, "flood t%d %d %lu\n", t, i, GetTickCount());
        WriteFile(out, b, n, &w, NULL);
        Sleep(10);
    }
    return 0;
}

int main(void) {
    HANDLE h[4];
    for (int t = 0; t < 4; t++)
        h[t] = CreateThread(NULL, 0, run, (void *)(INT_PTR)t, 0, NULL);
    WaitForMultipleObjects(4, h, TRUE, INFINITE);
    DWORD w;
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), "flood end\n", 10, &w, NULL);
    return 0;
}
