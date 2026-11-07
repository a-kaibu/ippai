/* The process environment: Set/GetEnvironmentVariable (sizes, removal, case, errors),
 * ExpandEnvironmentStrings, the order of GetEnvironmentStrings and the C runtime's getenv.
 * Only variables the probe sets are printed (the runner's own differ). */
#include "../probe.h"

static void aget(const char *name, DWORD size) {
    char buf[64];
    memset(buf, '#', sizeof buf);
    SetLastError(0xdead);
    DWORD n = GetEnvironmentVariableA(name, size ? buf : NULL, size);
    buf[sizeof buf - 1] = 0;
    printf("get %s size %lu -> %lu err %lu buf %.12s\n", name, size, n, GetLastError(), size && n && n < size ? buf : "-");
}

static void wget(const WCHAR *name, DWORD size) {
    WCHAR buf[64];
    for (int i = 0; i < 64; i++) buf[i] = '#';
    SetLastError(0xdead);
    DWORD n = GetEnvironmentVariableW(name, size ? buf : NULL, size);
    printf("getw size %lu -> %lu err %lu first %04x\n", size, n, GetLastError(), buf[0]);
}

static void set(const char *name, const char *value) {
    SetLastError(0xdead);
    BOOL ok = SetEnvironmentVariableA(name, value);
    printf("set %s=%s -> %d err %lu\n", name ? name : "(null)", value ? value : "(null)", ok, GetLastError());
}

static void expand(const char *s, DWORD size) {
    char buf[128];
    memset(buf, '#', sizeof buf);
    DWORD n = ExpandEnvironmentStringsA(s, buf, size);
    buf[sizeof buf - 1] = 0;
    printf("expand \"%s\" size %lu -> %lu \"%s\"\n", s, size, n, n && n <= size ? buf : "-");
}

int main(void) {
    set("PROBE_A", "alpha");
    aget("PROBE_A", 64);
    aget("probe_a", 64);
    aget("PROBE_A", 6);
    aget("PROBE_A", 5);
    aget("PROBE_A", 0);
    aget("PROBE_MISSING", 64);
    wget(L"PROBE_A", 64);
    wget(L"PROBE_A", 3);
    set("PROBE_A", "");
    aget("PROBE_A", 64);
    set("PROBE_A", NULL);
    aget("PROBE_A", 64);
    set("PROBE_NEVER", NULL);
    set("", "x");
    set("PROBE=B", "x");
    set("Probe_Mixed", "one");
    set("PROBE_MIXED", "two");
    aget("probe_mixed", 64);

    set("PROBE_Z", "z");
    set("PROBE_B", "b");
    set("PROBE_Y", "y");
    expand("%PROBE_B%-%PROBE_Y%", 128);
    expand("%PROBE_B%-%PROBE_Y%", 3);
    expand("%PROBE_NONE%x", 128);
    expand("%%PROBE_B%%", 128);
    expand("%PROBE_B", 128);
    expand("100%", 128);

    /* the probe's variables in the order the block lists them, and the spelling it keeps */
    char *block = GetEnvironmentStringsA();
    printf("block:");
    for (char *p = block; *p; p += strlen(p) + 1)
        if (!_strnicmp(p, "PROBE_", 6)) printf(" %s", p);
    printf("\n");
    FreeEnvironmentStringsA(block);
    WCHAR *wblock = GetEnvironmentStringsW();
    int n = 0;
    for (WCHAR *p = wblock; *p; p += wcslen(p) + 1)
        if (!_wcsnicmp(p, L"PROBE_", 6)) n++;
    printf("wblock probe vars %d\n", n);
    FreeEnvironmentStringsW(wblock);

    /* the C runtime reads the process environment as it was at startup (msvcrt keeps a copy):
     * set through kernel32 after that, getenv does not see it; _putenv does both */
    printf("getenv PROBE_B %s\n", getenv("PROBE_B") ? getenv("PROBE_B") : "(null)");
    printf("_putenv %d\n", _putenv("PROBE_C=see"));
    printf("getenv PROBE_C %s\n", getenv("PROBE_C") ? getenv("PROBE_C") : "(null)");
    aget("PROBE_C", 64);
    return 0;
}
