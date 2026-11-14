/* Small APIs trials stumbled on (2026-11, #209 #98 #222): shlwapi StrRStrIA/W (the last match
 * before a limit, case folded, a match may run past the limit), user32 IsImmersiveProcess,
 * setupapi's device information sets for a class with no devices (and the argument checks of
 * SetupDiEnumDeviceInfo / SetupDiDestroyDeviceInfoList), ole32 CoInitializeSecurity before and
 * after CoInitialize and twice, CoSetProxyBlanket on an object of the process (it has no
 * IClientSecurity), imm32 ImmGetDefaultIMEWnd before and after the thread's first window, and
 * shell32's AVI resources (the file copy dialog's clips: せきさば！'s installer plays 150).
 * libs: -lshlwapi -lole32 -lsetupapi -limm32 -luuid */
#define COBJMACROS
#define _WIN32_WINNT 0x0a00
#include "../probe.h"
#include <shlwapi.h>
#include <setupapi.h>
#include <imm.h>

static void rstr(const char *s, int last, const char *find) {
    const char *r = StrRStrIA(s, last < 0 ? NULL : s + last, find);
    printf("StrRStrIA(%s, %d, %s) = %d\n", s, last, find, r ? (int)(r - s) : -1);
    WCHAR ws[64], wf[64];
    MultiByteToWideChar(CP_ACP, 0, s, -1, ws, 64);
    MultiByteToWideChar(CP_ACP, 0, find, -1, wf, 64);
    const WCHAR *w = StrRStrIW(ws, last < 0 ? NULL : ws + last, wf);
    printf("StrRStrIW(%s, %d, %s) = %d\n", s, last, find, w ? (int)(w - ws) : -1);
}

/* an object with IUnknown only */
static HRESULT STDMETHODCALLTYPE qi(IUnknown *o, REFIID iid, void **out) {
    if (IsEqualIID(iid, &IID_IUnknown)) { *out = o; return S_OK; }
    *out = NULL;
    return E_NOINTERFACE;
}
static ULONG STDMETHODCALLTYPE ref(IUnknown *o) { return 1; }
static IUnknownVtbl vt = { qi, ref, ref };
static IUnknown obj = { &vt };

/* (the call, then GetLastError: C leaves the order of a call's arguments open) */
static void err(const char *what, BOOL r) {
    DWORD e = GetLastError();
    printf("%s %d %lu\n", what, r, e);
}

int main(void) {
    rstr("abcabcABC", -1, "abc");
    rstr("abcabcABC", 6, "abc");
    rstr("abcabcABC", 4, "abc");
    rstr("abcabcABC", 3, "abc");
    rstr("abcabcABC", 0, "abc");
    rstr("abcabcABC", -1, "x");
    rstr("abcabcABC", -1, "");
    rstr("aXbX", -1, "x");
    /* (DBCS: ア is 83 41, its trail byte is not the letter A) */
    rstr("\x83\x41" "a", -1, "A");
    rstr("\x83\x41\x83\x41", -1, "\x83\x41");
    printf("StrRStrIA NULLs %p %p\n", StrRStrIA(NULL, NULL, "a"), StrRStrIA("a", NULL, NULL));

    printf("IsImmersiveProcess %d\n", IsImmersiveProcess(GetCurrentProcess()));

    /* a class no device has */
    GUID none = { 0x5ab1e7a1, 0x1234, 0x4abc, { 0x9d, 0x01, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77 } };
    SetLastError(0xdead);
    HDEVINFO bad = SetupDiGetClassDevsW(NULL, NULL, NULL, 0);
    printf("GetClassDevs(NULL, 0) %s %lu\n", bad == INVALID_HANDLE_VALUE ? "INVALID_HANDLE_VALUE" : "a set", GetLastError());
    HDEVINFO all = SetupDiGetClassDevsW(NULL, NULL, NULL, DIGCF_ALLCLASSES | DIGCF_PRESENT);
    printf("GetClassDevs(NULL, ALLCLASSES) %s\n", all == INVALID_HANDLE_VALUE ? "INVALID_HANDLE_VALUE" : "a set");
    if (all != INVALID_HANDLE_VALUE) SetupDiDestroyDeviceInfoList(all);
    HDEVINFO set = SetupDiGetClassDevsW(&none, NULL, NULL, DIGCF_PRESENT);
    printf("GetClassDevs(none) %s\n", set == INVALID_HANDLE_VALUE ? "INVALID_HANDLE_VALUE" : "a set");
    SP_DEVINFO_DATA d = { 0 };
    SetLastError(0xdead);
    err("Enum(NULL set)", SetupDiEnumDeviceInfo(NULL, 0, &d));
    SetLastError(0xdead);
    err("Enum(NULL data)", SetupDiEnumDeviceInfo(set, 0, NULL));
    SetLastError(0xdead);
    err("Enum(cbSize 0)", SetupDiEnumDeviceInfo(set, 0, &d));
    d.cbSize = sizeof d;
    SetLastError(0xdead);
    err("Enum(0)", SetupDiEnumDeviceInfo(set, 0, &d));
    printf("Destroy %d\n", SetupDiDestroyDeviceInfoList(set));
    SetLastError(0xdead);
    err("Destroy(NULL)", SetupDiDestroyDeviceInfoList(NULL));

    printf("CoInitializeSecurity before COM %08lx\n", (unsigned long)CoInitializeSecurity(NULL, -1, NULL, NULL, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE, NULL, EOAC_NONE, NULL));
    CoInitialize(NULL);
    printf("CoInitializeSecurity %08lx\n", (unsigned long)CoInitializeSecurity(NULL, -1, NULL, NULL, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE, NULL, EOAC_NONE, NULL));
    printf("CoInitializeSecurity again %08lx\n", (unsigned long)CoInitializeSecurity(NULL, -1, NULL, NULL, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE, NULL, EOAC_NONE, NULL));
    printf("CoSetProxyBlanket(in-process) %08lx\n", (unsigned long)CoSetProxyBlanket(&obj, RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE, NULL, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE, NULL, EOAC_NONE));
    CoUninitialize();

    printf("ImmGetDefaultIMEWnd before a window %d\n", ImmGetDefaultIMEWnd(NULL) != NULL);
    HWND h = CreateWindowExA(0, "STATIC", "w", WS_POPUP, 0, 0, 10, 10, NULL, NULL, NULL, NULL);
    HWND ime = ImmGetDefaultIMEWnd(NULL);
    char cls[32] = "";
    if (ime) GetClassNameA(ime, cls, sizeof cls);
    printf("ImmGetDefaultIMEWnd %d class %s visible %d same %d\n", ime != NULL, cls, ime ? IsWindowVisible(ime) : -1, ImmGetDefaultIMEWnd(h) == ime);
    DestroyWindow(h);

    HMODULE sh = LoadLibraryA("shell32.dll");
    int ids[] = { 150, 160, 161 };
    for (int i = 0; i < 3; i++) {
        HRSRC r = FindResourceA(sh, MAKEINTRESOURCEA(ids[i]), "AVI");
        DWORD n = r ? SizeofResource(sh, r) : 0;
        const char *p = r ? LockResource(LoadResource(sh, r)) : NULL;
        printf("shell32 AVI %d: %s, %s\n", ids[i], r ? "found" : "missing", p && n > 12 && !memcmp(p, "RIFF", 4) && !memcmp(p + 8, "AVI ", 4) && *(DWORD *)(p + 4) + 8 <= n ? "RIFF AVI" : "?");
    }
    return 0;
}
