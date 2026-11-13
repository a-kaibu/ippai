/* msvcrt's wide printf to streams: fwprintf, fwprintf_s, vfwprintf, vwprintf (taken from
 * msvcrt.dll itself, not mingw's own). Their counts, stdout, and a text file read back as bytes
 * (wide output goes out as the locale's bytes). sake had none of them: mingw's wprintf calls
 * fwprintf. The expected lines are Windows'. */
#include <windows.h>
#include <stdio.h>
#include <stdarg.h>
#include <wchar.h>
typedef int (__cdecl *FW)(FILE *, const wchar_t *, ...);
typedef int (__cdecl *VFW)(FILE *, const wchar_t *, va_list);
typedef int (__cdecl *VW)(const wchar_t *, va_list);
static VFW vfw;
static VW vw;
static int call_vfw(FILE *f, const wchar_t *fmt, ...) { va_list a; va_start(a, fmt); int n = vfw(f, fmt, a); va_end(a); return n; }
static int call_vw(const wchar_t *fmt, ...) { va_list a; va_start(a, fmt); int n = vw(fmt, a); va_end(a); return n; }
int main(void) {
    HMODULE m = GetModuleHandleA("msvcrt.dll");
    FW fw = (FW)GetProcAddress(m, "fwprintf"), fws = (FW)GetProcAddress(m, "fwprintf_s");
    vfw = (VFW)GetProcAddress(m, "vfwprintf");
    vw = (VW)GetProcAddress(m, "vwprintf");
    printf("found %d %d %d %d\n", !!fw, !!fws, !!vfw, !!vw);
    fflush(stdout);
    int n1 = fw(stdout, L"fwprintf %ls %d %c\n", L"wide", 42, L'x');
    int n2 = fws(stdout, L"fwprintf_s %5.2f|%-4d|\n", 3.14159, 7);
    int n3 = call_vfw(stdout, L"vfwprintf %x %ls\n", 0xbeef, L"ok");
    int n4 = call_vw(L"vwprintf %03d\n", 5);
    fflush(stdout);
    printf("counts %d %d %d %d\n", n1, n2, n3, n4);
    char path[MAX_PATH];
    GetTempPathA(MAX_PATH, path);
    strcat(path, "wideprintf.txt");
    FILE *f = fopen(path, "w");
    int n5 = fw(f, L"a%lsb\n", L"ZZ");
    int n6 = call_vfw(f, L"%d;\n", -12);
    fclose(f);
    f = fopen(path, "rb");
    unsigned char b[64];
    size_t len = fread(b, 1, sizeof b, f);
    fclose(f);
    DeleteFileA(path);
    printf("file counts %d %d bytes %u:", n5, n6, (unsigned)len);
    for (size_t i = 0; i < len; i++) printf(" %02x", b[i]);
    printf("\n");
    return 0;
}
