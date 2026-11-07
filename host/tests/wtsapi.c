/* wtsapi32 (loaded at run time, as FVP's and Yuka's trials do) and bcryptprimitives!ProcessPrng:
 * session notifications, the current session's state, id and protocol, the session list, and
 * random bytes. Prints only what any desktop Windows session gives (not names or ids); built
 * for x86 and x86-64. No C runtime.
 * libs: -nostdlib -luser32 -lkernel32 */
#include <windows.h>

static void out(const char *f, ...)
{
    char b[512];
    DWORD n;
    va_list a;
    va_start(a, f);
    n = wvsprintfA(b, f, a);
    va_end(a);
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), b, n, &n, NULL);
}

typedef struct { DWORD SessionId; char *pWinStationName; int State; } SESSION_A;
typedef struct { DWORD SessionId; WCHAR *pWinStationName; int State; } SESSION_W;

void mainCRTStartup(void)
{
    HMODULE w = LoadLibraryA("wtsapi32.dll");
    out("load %d\n", w != NULL);
    if (!w) ExitProcess(1);
    BOOL(WINAPI *reg)(HWND, DWORD) = (void *)GetProcAddress(w, "WTSRegisterSessionNotification");
    BOOL(WINAPI *unreg)(HWND) = (void *)GetProcAddress(w, "WTSUnRegisterSessionNotification");
    BOOL(WINAPI *qa)(HANDLE, DWORD, int, char **, DWORD *) = (void *)GetProcAddress(w, "WTSQuerySessionInformationA");
    BOOL(WINAPI *qw)(HANDLE, DWORD, int, WCHAR **, DWORD *) = (void *)GetProcAddress(w, "WTSQuerySessionInformationW");
    BOOL(WINAPI *ea)(HANDLE, DWORD, DWORD, SESSION_A **, DWORD *) = (void *)GetProcAddress(w, "WTSEnumerateSessionsA");
    BOOL(WINAPI *ew)(HANDLE, DWORD, DWORD, SESSION_W **, DWORD *) = (void *)GetProcAddress(w, "WTSEnumerateSessionsW");
    void(WINAPI *freem)(void *) = (void *)GetProcAddress(w, "WTSFreeMemory");
    out("procs %d\n", reg && unreg && qa && qw && ea && ew && freem);

    HWND h = CreateWindowExA(0, "STATIC", "", 0, 0, 0, 10, 10, NULL, NULL, NULL, NULL);
    out("register %d unregister %d\n", reg(h, 0), unreg(h));

    DWORD session = 99, n = 0;
    out("process session %d\n", ProcessIdToSessionId(GetCurrentProcessId(), &session));
    char *p = NULL;
    WCHAR *q = NULL;
    /* WTSConnectState: WTSActive; WTSSessionId: ours; WTSClientProtocolType: the console */
    BOOL r = qa(NULL, (DWORD)-1, 8, &p, &n);
    out("state %d %d bytes %u\n", r, r ? *(int *)p : -1, n);
    freem(p);
    r = qw(NULL, (DWORD)-1, 4, &q, &n);
    out("id %d same %d bytes %u\n", r, r && *(DWORD *)q == session, n);
    freem(q);
    r = qa(NULL, session, 16, &p, &n);
    out("protocol %d %d bytes %u\n", r, r ? *(USHORT *)p : -1, n);
    freem(p);
    /* names: strings, their size in bytes with the terminator */
    r = qw(NULL, (DWORD)-1, 5, &q, &n);
    out("user %d sized %d\n", r, r && n == 2 * (DWORD)lstrlenW(q) + 2 && n > 2);
    freem(q);
    r = qa(NULL, (DWORD)-1, 6, &p, &n);
    out("station %d sized %d\n", r, r && n == (DWORD)lstrlenA(p) + 1);
    freem(p);
    p = (char *)1;
    r = qa(NULL, (DWORD)-1, 1000, &p, &n);
    out("bad class %d\n", r);

    SESSION_A *sa = NULL;
    SESSION_W *sw = NULL;
    DWORD c = 0, found = 0;
    r = ea(NULL, 0, 1, &sa, &c);
    for (DWORD i = 0; r && i < c; i++)
        if (sa[i].SessionId == session) found = sa[i].State == 0 && sa[i].pWinStationName != NULL;
    out("enum %d ours %d\n", r, found);
    freem(sa);
    found = 0;
    r = ew(NULL, 0, 1, &sw, &c);
    for (DWORD i = 0; r && i < c; i++)
        if (sw[i].SessionId == session) found = sw[i].State == 0 && sw[i].pWinStationName != NULL;
    out("enumw %d ours %d\n", r, found);
    freem(sw);
    out("enum version 2 %d\n", ea(NULL, 0, 2, &sa, &c));

    HMODULE b = LoadLibraryA("bcryptprimitives.dll");
    BOOL(WINAPI *prng)(BYTE *, SIZE_T) = b ? (void *)GetProcAddress(b, "ProcessPrng") : NULL;
    out("prng %d\n", prng != NULL);
    if (prng) {
        BYTE x[64], y[64];
        int same = 1;
        BOOL r1 = prng(x, sizeof x), r2 = prng(y, sizeof y);
        for (int i = 0; i < 64; i++) same &= x[i] == y[i];
        out("prng %d %d differ %d\n", r1, r2, !same);
    }
    ExitProcess(0);
}
