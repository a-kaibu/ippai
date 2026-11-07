/* printf with positional arguments (#43): msvcrt's _sprintf_p_l / _scprintf_p_l and the UCRT's
 * __stdio_common_vsprintf_p (what _sprintf_p in the UCRT headers calls), both found at run time.
 * "%n$" picks argument n, "*n$" a width or precision from one; arguments of either size and
 * doubles, in any order and used more than once; then what is refused: a mix with plain
 * conversions, a gap, position 0, a position read as two types, a * without its position, and
 * a result that does not fit (each line: the result and the buffer). */
#include "../probe.h"
#include <stdarg.h>

typedef int(__cdecl *sp_l)(char *, size_t, const char *, _locale_t, ...);
typedef int(__cdecl *scp_l)(const char *, _locale_t, ...);
typedef int(__cdecl *common)(unsigned __int64, char *, size_t, const char *, _locale_t, va_list);
static sp_l msp;
static scp_l msc;
static common ucp;

static void __cdecl inv(const wchar_t *e, const wchar_t *f, const wchar_t *file, unsigned line, uintptr_t r) {}

static int uc(char *b, size_t n, const char *f, _locale_t l, ...) {
    va_list a;
    va_start(a, l);
    int r = ucp(0, b, n, f, NULL, a);
    va_end(a);
    return r;
}

#define P(size, ...) do { \
    char b[256]; memset(b, 'x', sizeof b); b[255] = 0; \
    if (msp) { int n = msp(b, size, __VA_ARGS__); printf("msvcrt %-30s %d |%s|\n", #__VA_ARGS__, n, n < 0 ? (b[0] ? "?" : "") : b); } \
    memset(b, 'x', sizeof b); b[255] = 0; \
    if (ucp) { int n = uc(b, size, __VA_ARGS__); printf("ucrt   %-30s %d |%s|\n", #__VA_ARGS__, n, n < 0 ? (b[0] ? "?" : "") : b); } \
} while (0)
#define L NULL

int main(void) {
    HMODULE m = LoadLibraryA("msvcrt.dll"), u = LoadLibraryA("ucrtbase.dll");
    msp = (sp_l)GetProcAddress(m, "_sprintf_p_l");
    msc = (scp_l)GetProcAddress(m, "_scprintf_p_l");
    ucp = u ? (common)GetProcAddress(u, "__stdio_common_vsprintf_p") : NULL;
    printf("msvcrt _sprintf_p_l %d _scprintf_p_l %d _sprintf_p %d; ucrt %d\n", !!msp, !!msc, !!GetProcAddress(m, "_sprintf_p"), !!ucp);
    _set_invalid_parameter_handler(inv);
    if (u) {
        typedef _invalid_parameter_handler(__cdecl *seth)(_invalid_parameter_handler);
        seth s = (seth)GetProcAddress(u, "_set_invalid_parameter_handler");
        if (s) s(inv);
    }
    P(256, "%1$s %2$s", L, "November", "10");
    P(256, "%2$s %1$s", L, "November", "10");
    P(256, "%2$d %1$d %2$d", L, 7, 9);
    P(256, "%2$.2f/%1$d", L, 3, 2.5);
    P(256, "%1$.2f/%2$I64x", L, 0.25, (__int64)1 << 40);
    P(256, "%3$I64d %1$d %2$.1f", L, -5, 0.5, (__int64)-1);
    P(256, "[%2$*1$d]", L, 5, 42);
    P(256, "[%3$-*1$.*2$f]", L, 8, 1, 3.14159);
    P(256, "100%% %1$d", L, 5);
    P(256, "%1$c%1$c%1$c", L, 'z');
    P(256, "%d %s", L, 1, "plain");
    P(256, "%1$d %d", L, 1, 2);
    P(256, "%d %1$d", L, 1, 2);
    P(256, "%1$d %3$d", L, 1, 2, 3);
    P(256, "%0$d", L, 1);
    P(256, "%1$d %1$f", L, 1, 2.0);
    P(256, "%2$*d", L, 1, 2);
    P(256, "%*1$d", L, 1, 2);
    P(4, "%1$s", L, "abcdef");
    P(7, "%1$s", L, "abcdef");
    if (msc) printf("msvcrt _scprintf_p_l %d %d\n", msc("%2$s-%1$d", NULL, 12, "ab"), msc("%1$d %d", NULL, 1, 2));
    return 0;
}
