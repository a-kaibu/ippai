/* kernel32 FormatMessage from strings (inserts %1..%99 with printf specs !d! !5s! !*d!, %n %t
 * %0 %% %. %r, FORMAT_MESSAGE_MAX_WIDTH_MASK line breaking, ARGUMENT_ARRAY, IGNORE_INSERTS,
 * ALLOCATE_BUFFER, buffer-too-small errors, A/W), the system messages' error answers, and
 * module/process facts games read: GetModuleHandle/LoadLibrary/GetProcAddress errors and
 * ordinals, GetModuleFileName truncation, GetCommandLine/argv basics, GetStartupInfo. */
#include "../probe.h"

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-72s = %lld err %lu\n", #expr, r_, GetLastError()); } while (0)

static void hexs(const char *s, int n) {
    printf("[");
    for (int i = 0; i < n; i++) {
        unsigned char c = s[i];
        if (c == '\r') printf("\\r");
        else if (c == '\n') printf("\\n");
        else if (c < 32 || c >= 127) printf("\\x%02x", c);
        else putchar(c);
    }
    printf("]");
}

static void fa(const char *fmt, DWORD flags, DWORD cap, ...) {
    char b[512];
    memset(b, '#', sizeof b);
    va_list ap;
    va_start(ap, cap);
    SetLastError(0xdead);
    DWORD n = FormatMessageA(FORMAT_MESSAGE_FROM_STRING | flags, fmt, 0, 0, b, cap, &ap);
    DWORD e = GetLastError();
    va_end(ap);
    printf("fmt ");
    hexs(fmt, strlen(fmt));
    printf(" flags %lx cap %lu: %lu err %lu ", flags, cap, n, n ? 0 : e);
    hexs(b, n ? n : 0);
    printf(" next %02x\n", (unsigned char)b[n]);
}

static void fw(const WCHAR *fmt, DWORD flags, ...) {
    WCHAR b[256];
    va_list ap;
    va_start(ap, flags);
    SetLastError(0xdead);
    DWORD n = FormatMessageW(FORMAT_MESSAGE_FROM_STRING | flags, fmt, 0, 0, b, 256, &ap);
    va_end(ap);
    printf("fmtW %lu err %lu:", n, n ? 0 : GetLastError());
    for (DWORD i = 0; i < n; i++) printf(" %04x", b[i]);
    printf("\n");
}

int main(void) {
    fa("plain text", 0, 64);
    fa("a %1 b %2 c", 0, 64, "one", "two");
    fa("%2 before %1", 0, 64, "one", "two");
    fa("%1!d! %2!5d! %3!-5d!| %4!x! %5!08X!", 0, 64, 42, 7, 3, 255, 0xbeef);
    fa("%1!s! %2!.3s! %3!5s!|", 0, 64, "abc", "abcdef", "ab");
    fa("%1!*d!|", 0, 64, 6, 12);
    fa("%1!c! %2!u! %3!ld!", 0, 64, 'Z', -1, 5L);
    /* precisions, '#' and zero padding after 0x, a precision of 0 for 0, widths from '*' */
    fa("%1!.5u! %2!#.3o! %3!#o! %4!0#11x! %5!.0d!|%6!#X!", 0, 64, 42, 1, 8, 1, 0, 0);
    fa("%1!-+8d!|%2! 05d!|%3!+.4i!|%4!*.*d!|", 0, 64, 42, -42, 7, 9, 4, 3);
    fa("line1%nline2%r%t|%0 after zero", 0, 64);
    fa("percent %% dot %. bang %!", 0, 64);
    fa("end\r\n", 0, 64);
    fa("trailing %", 0, 64);
    fa("%1", FORMAT_MESSAGE_IGNORE_INSERTS, 64);
    fa("%1 and %2!d!", FORMAT_MESSAGE_IGNORE_INSERTS, 64);
    fa("the quick brown fox jumps over the lazy dog again and again", 20, 128);
    fa("the quick brown fox\r\njumps", FORMAT_MESSAGE_MAX_WIDTH_MASK, 128);
    fa("  spaced   words  ", FORMAT_MESSAGE_MAX_WIDTH_MASK, 128);
    fa("verylongwordwithoutanyspaces here", 10, 128);
    fa("hello world", 0, 5);
    fa("hello world", 0, 11);
    fa("hello world", 0, 12);
    fa("%1", 0, 3, "long insert");

    /* argument arrays */
    DWORD_PTR args[] = { (DWORD_PTR)"first", 17, (DWORD_PTR)"third" };
    char b[256];
    DWORD n = FormatMessageA(FORMAT_MESSAGE_FROM_STRING | FORMAT_MESSAGE_ARGUMENT_ARRAY, "%1 %2!d! %3 %1", 0, 0, b, sizeof b, (va_list *)args);
    printf("array %lu ", n); hexs(b, n); printf("\n");
    char *alloc = NULL;
    n = FormatMessageA(FORMAT_MESSAGE_FROM_STRING | FORMAT_MESSAGE_ARGUMENT_ARRAY | FORMAT_MESSAGE_ALLOCATE_BUFFER, "allocated %1", 0, 0, (char *)&alloc, 0, (va_list *)args);
    printf("alloc %lu ", n); hexs(alloc, n); printf(" local %d\n", LocalFree(alloc) == NULL);
    fw(L"wide %1!s! %2!d!", 0, L"\x65e5\x672c", 9);
    fw(L"%1!S!", 0, "ansi in wide");

    /* system messages: whether they exist and the errors (their text is the runner's language) */
    char sm[512];
    E(FormatMessageA(FORMAT_MESSAGE_FROM_SYSTEM, NULL, 2, 0, sm, sizeof sm, NULL) > 0);
    E(FormatMessageA(FORMAT_MESSAGE_FROM_SYSTEM, NULL, 0x12345678, 0, sm, sizeof sm, NULL));
    E(FormatMessageA(FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS, NULL, 2, 0, sm, 3, NULL));
    E(FormatMessageA(FORMAT_MESSAGE_FROM_SYSTEM, NULL, 2, MAKELANGID(LANG_ZULU, SUBLANG_DEFAULT), sm, sizeof sm, NULL));
    E(FormatMessageA(0, NULL, 2, 0, sm, sizeof sm, NULL));
    /* RPG Maker VX's Game.exe (RGSS2 Player) reports a missing RGSS202J.dll this way:
     * STG_E_FILENOTFOUND from the system with the dll's name as %1 in an argument array, in the
     * user's default language (empty there = an empty message box) */
    {
        WCHAR wm[0x800];
        DWORD_PTR stg[] = { (DWORD_PTR)L"RGSS202J.dll" };
        SetLastError(0xdead);
        DWORD n = FormatMessageW(FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_ARGUMENT_ARRAY, NULL, 0x80030002, LANG_USER_DEFAULT, wm, 0x800, (va_list *)stg);
        printf("stg filenotfound %d err %lu insert %d crlf %d\n", n > 0, n ? 0 : GetLastError(), n && wcsstr(wm, L"RGSS202J.dll") != NULL, n >= 2 && wm[n - 2] == '\r' && wm[n - 1] == '\n');
    }

    /* modules */
    E(GetModuleHandleA(NULL) != NULL);
    E(GetModuleHandleA("kernel32") != NULL);
    E(GetModuleHandleA("KERNEL32.DLL") == GetModuleHandleA("kernel32.dll"));
    E(GetModuleHandleA("kernel32.") != NULL);
    E(GetModuleHandleA("nosuch.dll") != NULL);
    E(LoadLibraryA("nosuch.dll") != NULL);
    E(LoadLibraryA("") != NULL);
    HMODULE k = GetModuleHandleA("kernel32.dll");
    E(LoadLibraryA("kernel32.dll") == k);
    E(GetProcAddress(k, "NoSuchFunction") != NULL);
    E(GetProcAddress(k, "getlasterror") != NULL);
    E(GetProcAddress(k, "GetLastError") == (FARPROC)GetLastError);
    E(GetProcAddress(k, MAKEINTRESOURCEA(60000)) != NULL);
    E(GetProcAddress(NULL, "GetLastError") != NULL);
    E(GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, (LPCSTR)main, &k) && k == GetModuleHandleA(NULL));
    E(FreeLibrary(NULL));
    char p[MAX_PATH], small[8];
    DWORD full = GetModuleFileNameA(NULL, p, sizeof p);
    const char *base = strrchr(p, '\\');
    printf("module file name ends [%s] length matches %d\n", base ? base + 1 : p, full == strlen(p));
    memset(small, '#', sizeof small);
    SetLastError(0xdead);
    DWORD r = GetModuleFileNameA(NULL, small, sizeof small);
    printf("truncated %lu err %lu nul at end %d\n", r, GetLastError(), small[7] == 0);
    E(GetModuleFileNameA((HMODULE)(ULONG_PTR)0x12340000, p, sizeof p));
    WCHAR wp[4];
    SetLastError(0xdead);
    r = GetModuleFileNameW(NULL, wp, 4);
    printf("truncated W %lu err %lu last %04x\n", r, GetLastError(), wp[3]);

    /* the command line as run.sh starts the exe (no arguments) */
    const char *cl = GetCommandLineA();
    printf("command line quoted %d ends with exe %d\n", cl[0] == '"', strstr(cl, ".exe") != NULL);
    STARTUPINFOA si;
    memset(&si, 0xcc, sizeof si);
    GetStartupInfoA(&si);
    printf("startupinfo cb %lu reserved null %d desktop set %d flags-show %d\n", si.cb, si.lpReserved == NULL, si.lpDesktop != NULL, (si.dwFlags & STARTF_USESHOWWINDOW) != 0);
    E(GetCurrentProcessId() == GetProcessId(GetCurrentProcess()));
    E(GetCurrentThreadId() == GetThreadId(GetCurrentThread()));
    E(GetPriorityClass(GetCurrentProcess()));
    DWORD code = 0;
    E(GetExitCodeProcess(GetCurrentProcess(), &code));
    printf("exit code while running %lu\n", code);
    return 0;
}
