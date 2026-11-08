/* UCRT printf's %T size (the output function's character width, even for %TS/%TC) and other
 * forms only ucrtbase has, through __stdio_common_vs(w)printf (the probes' printf is msvcrt's). */
#include "probe.h"
#include <stdarg.h>
#include <wchar.h>
typedef int (__cdecl *vsp)(unsigned long long, char *, size_t, const char *, void *, va_list);
typedef int (__cdecl *vswp)(unsigned long long, wchar_t *, size_t, const wchar_t *, void *, va_list);
static vsp a; static vswp w;
static void fa(const char *label, const char *f, ...) {
    char out[128]; va_list v; va_start(v, f);
    int n = a(0, out, sizeof out, f, NULL, v); va_end(v);
    printf("%s: %d [%s]\n", label, n, n >= 0 ? out : "");
}
static void fw(const char *label, const wchar_t *f, ...) {
    wchar_t out[128]; va_list v; va_start(v, f);
    int n = w(0, out, 128, f, NULL, v); va_end(v);
    printf("%s: %d [", label, n);
    for (int i = 0; i < n && i < 128; i++) printf("%04x", out[i]);
    printf("]\n");
}
int main(void) {
    HMODULE u = LoadLibraryA("ucrtbase.dll");
    a = u ? (vsp)GetProcAddress(u, "__stdio_common_vsprintf") : NULL;
    w = u ? (vswp)GetProcAddress(u, "__stdio_common_vswprintf") : NULL;
    if (!a || !w) { puts("skip no ucrtbase"); return 0; }
    fa("narrow Ts Tc", "%Ts|%TS|%Tc|%TC", "ab", "cd", 'e', 'f');
    fw("wide Ts Tc", L"%Ts|%TS|%Tc|%TC", L"\x3042", L"\x3044", 0x3046, 0x3048);
    fa("narrow s S c C", "%s|%S|%c|%C", "ab", L"cd", 'e', L'f');
    fw("wide s S c C", L"%s|%S|%c|%C", L"ab", "cd", L'e', 'f');
    fa("T width", "[%5Ts][%-3Tc]", "x", 'y');
    fa("hs ls", "%hs|%ls|%hc|%lc", "ab", L"cd", 'e', L'f');
    return 0;
}
