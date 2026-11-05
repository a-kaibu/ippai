#define __USE_MINGW_ANSI_STDIO 0
#include <windows.h>
#include <stdio.h>
static void check(const char *name, int result) {
    printf("%s %d %lu\n", name, result, (unsigned long)GetLastError());
}
int main(void) {
    WCHAR w[4]; char b[4];
    SetLastError(0); check("mb-zero", MultiByteToWideChar(CP_UTF8, 0, "x", 0, w, 4));
    SetLastError(0); check("mb-length", MultiByteToWideChar(CP_UTF8, 0, "x", -2, w, 4));
    SetLastError(0); check("mb-null", MultiByteToWideChar(CP_UTF8, 0, NULL, 1, w, 4));
    SetLastError(0); check("mb-output", MultiByteToWideChar(CP_UTF8, 0, "x", 1, NULL, 1));
    SetLastError(0); check("wc-zero", WideCharToMultiByte(CP_UTF8, 0, L"x", 0, b, 4, NULL, NULL));
    SetLastError(0); check("wc-length", WideCharToMultiByte(CP_UTF8, 0, L"x", -2, b, 4, NULL, NULL));
    SetLastError(0); check("wc-null", WideCharToMultiByte(CP_UTF8, 0, NULL, 1, b, 4, NULL, NULL));
    SetLastError(0); check("wc-output", WideCharToMultiByte(CP_UTF8, 0, L"x", 1, NULL, 1, NULL, NULL));
    printf("sizes %d %d\n", MultiByteToWideChar(CP_UTF8, 0, "x", -1, NULL, 0), WideCharToMultiByte(CP_UTF8, 0, L"x", -1, NULL, 0, NULL, NULL));
    return 0;
}
