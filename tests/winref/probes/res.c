/* Resources of the exe itself (probes/res.rc), as games load their strings, menus, dialogs and
 * data: FindResource(Ex) by id, name and language, Sizeof/Load/LockResource, EnumResourceTypes/
 * Names/Languages order, LoadString A/W (truncation, the read-only pointer form, empty and
 * missing strings, languages), LoadMenu items and states, LoadAccelerators, dialog templates,
 * GetFileVersionInfo / VerQueryValue on the exe, and the error codes of all of them.
 * libs: -lversion */
#include "../probe.h"

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-70s = %lld err %lu\n", #expr, r_, GetLastError()); } while (0)

static void hex(const void *p, int n) { for (int i = 0; i < n; i++) printf("%02x", ((const unsigned char *)p)[i]); }
static const char *name(LPCSTR n, char *buf) {
    if (IS_INTRESOURCE(n)) sprintf(buf, "#%u", (unsigned)(ULONG_PTR)n);
    else strcpy(buf, n);
    return buf;
}

static BOOL CALLBACK lang_cb(HMODULE m, LPCSTR t, LPCSTR n, WORD l, LONG_PTR p) { printf(" %04x", l); return TRUE; }
static BOOL CALLBACK name_cb(HMODULE m, LPCSTR t, LPSTR n, LONG_PTR p) {
    char b[64];
    printf("  name %s langs", name(n, b));
    EnumResourceLanguagesA(m, t, n, lang_cb, 0);
    printf("\n");
    return TRUE;
}
static BOOL CALLBACK type_cb(HMODULE m, LPSTR t, LONG_PTR p) {
    char b[64];
    printf("type %s\n", name(t, b));
    EnumResourceNamesA(m, t, name_cb, 0);
    return TRUE;
}
static BOOL CALLBACK stop_cb(HMODULE m, LPSTR t, LONG_PTR p) { printf("stop at first type\n"); return FALSE; }

static void res(LPCSTR n, LPCSTR t, WORD lang) {
    char b1[64], b2[64];
    SetLastError(0xdead);
    HRSRC r = lang == 0xffff ? FindResourceA(NULL, n, t) : FindResourceExA(NULL, t, n, lang);
    DWORD e = GetLastError();
    printf("find %s %s lang %x: %d err %lu", name(n, b1), name(t, b2), lang, r != NULL, r ? 0 : e);
    if (r) {
        DWORD sz = SizeofResource(NULL, r);
        HGLOBAL g = LoadResource(NULL, r);
        printf(" size %lu bytes ", sz);
        hex(LockResource(g), sz > 24 ? 24 : sz);
    }
    printf("\n");
}

static void strA(UINT id, int cap) {
    char b[64];
    memset(b, '#', sizeof b);
    SetLastError(0xdead);
    int n = LoadStringA(NULL, id, b, cap);
    printf("LoadStringA %u cap %d: %d err %lu [", id, cap, n, GetLastError());
    if (cap > 0) hex(b, n + 1 < (int)sizeof b ? n + 1 : (int)sizeof b);
    printf("] next %02x\n", (unsigned char)b[cap > 0 && cap < 64 ? cap : 63]);
}
static void strW(UINT id, int cap) {
    WCHAR b[64];
    for (int i = 0; i < 64; i++) b[i] = 0x2323;
    SetLastError(0xdead);
    int n = LoadStringW(NULL, id, b, cap);
    printf("LoadStringW %u cap %d: %d err %lu [", id, cap, n, GetLastError());
    if (cap > 0) for (int i = 0; i <= n && i < 64; i++) printf("%04x", b[i]);
    else if (cap == 0 && n > 0) {
        /* the pointer form: a read-only pointer to the counted string, not terminated */
        const WCHAR *p = *(const WCHAR **)b;
        for (int i = 0; i < n; i++) printf("%04x", p[i]);
        printf(" after %04x", p[n]);
    }
    printf("]\n");
}

static void item(HMENU m, UINT pos) {
    MENUITEMINFOA mi = { sizeof mi };
    char t[64] = "";
    mi.fMask = MIIM_ID | MIIM_STATE | MIIM_SUBMENU | MIIM_FTYPE | MIIM_STRING;
    mi.dwTypeData = t;
    mi.cch = sizeof t;
    BOOL r = GetMenuItemInfoA(m, pos, TRUE, &mi);
    printf("  item %u: %d id %u type %x state %x sub %d text [%s] cch %u\n", pos, r, mi.wID, mi.fType, mi.fState, mi.hSubMenu != NULL, t, mi.cch);
}

int main(void) {
    EnumResourceTypesA(NULL, type_cb, 0);
    EnumResourceTypesA(NULL, stop_cb, 0);

    res(MAKEINTRESOURCEA(200), RT_RCDATA, 0xffff);
    res(MAKEINTRESOURCEA(201), RT_RCDATA, 0xffff);
    res("NAMED", RT_RCDATA, 0xffff);
    res("named", RT_RCDATA, 0xffff);
    res("#200", RT_RCDATA, 0xffff);
    res(MAKEINTRESOURCEA(300), "MYTYPE", 0xffff);
    res(MAKEINTRESOURCEA(300), "mytype", 0xffff);
    res("LOWER", "MYTYPE", 0xffff);
    res(MAKEINTRESOURCEA(999), RT_RCDATA, 0xffff);
    res(MAKEINTRESOURCEA(200), "NOTYPE", 0xffff);
    res(MAKEINTRESOURCEA(200), RT_RCDATA, MAKELANGID(LANG_NEUTRAL, SUBLANG_NEUTRAL));
    res(MAKEINTRESOURCEA(200), RT_RCDATA, MAKELANGID(LANG_JAPANESE, SUBLANG_DEFAULT));
    res(MAKEINTRESOURCEA(200), RT_RCDATA, MAKELANGID(LANG_ENGLISH, SUBLANG_ENGLISH_US));
    res(MAKEINTRESOURCEA(1), RT_STRING, 0xffff);
    res(MAKEINTRESOURCEA(2), RT_STRING, 0xffff);
    res(MAKEINTRESOURCEA(1), RT_STRING, MAKELANGID(LANG_JAPANESE, SUBLANG_DEFAULT));
    res(MAKEINTRESOURCEA(1), RT_VERSION, 0xffff);
    E(SizeofResource(NULL, NULL));
    E(LoadResource(NULL, NULL) != NULL);
    E(LockResource(NULL) != NULL);
    E(FreeResource(NULL));

    strA(1, 64); strA(1, 3); strA(1, 1); strA(1, 0);
    strA(2, 64); strA(2, 8); strA(2, 9);
    strA(3, 64); strA(4, 64); strA(5, 64); strA(17, 64); strA(100, 10); strA(9999, 64);
    strW(1, 64); strW(1, 3); strW(1, 1); strW(1, 0);
    strW(2, 64); strW(2, 4); strW(2, 0);
    strW(3, 64); strW(3, 0); strW(4, 64); strW(5, 64); strW(17, 64); strW(100, 10); strW(9999, 64); strW(9999, 0);

    HMENU m = LoadMenuA(NULL, MAKEINTRESOURCEA(400));
    printf("menu %d count %d\n", m != NULL, GetMenuItemCount(m));
    for (int i = 0; i < 3; i++) item(m, i);
    HMENU sub = GetSubMenu(m, 0);
    printf("sub count %d id0 %u id1 %d state2 %x\n", GetMenuItemCount(sub), GetMenuItemID(sub, 0), (int)GetMenuItemID(sub, 1), GetMenuState(sub, 2, MF_BYPOSITION));
    for (int i = 0; i < 3; i++) item(sub, i);
    char mt[64] = "";
    printf("GetMenuStringA %d [%s]\n", GetMenuStringA(m, 403, mt, sizeof mt, MF_BYCOMMAND), mt);
    printf("GetMenuState 403 %x 401 %x 999 %x\n", GetMenuState(m, 403, MF_BYCOMMAND), GetMenuState(m, 401, MF_BYCOMMAND), GetMenuState(m, 999, MF_BYCOMMAND));
    printf("CheckMenuItem %lx %lx\n", CheckMenuItem(m, 401, MF_CHECKED), CheckMenuItem(m, 401, MF_UNCHECKED));
    printf("EnableMenuItem %d %d\n", EnableMenuItem(m, 402, MF_ENABLED), EnableMenuItem(m, 999, MF_GRAYED));
    E(DestroyMenu(m));
    E(LoadMenuA(NULL, MAKEINTRESOURCEA(401)) != NULL);

    HACCEL a = LoadAcceleratorsA(NULL, MAKEINTRESOURCEA(600));
    ACCEL acc[4];
    int na = CopyAcceleratorTableA(a, acc, 4);
    printf("accel %d count %d", a != NULL, na);
    for (int i = 0; i < na; i++) printf(" [%x %x %u]", acc[i].fVirt, acc[i].key, acc[i].cmd);
    printf(" count only %d\n", CopyAcceleratorTableA(a, NULL, 0));

    HRSRC dr = FindResourceA(NULL, MAKEINTRESOURCEA(500), RT_DIALOG);
    const WORD *dt = LockResource(LoadResource(NULL, dr));
    printf("dialog size %lu head ", SizeofResource(NULL, dr));
    hex(dt, 26);
    printf("\n");

    char path[MAX_PATH];
    GetModuleFileNameA(NULL, path, sizeof path);
    DWORD hnd = 7, vs = GetFileVersionInfoSizeA(path, &hnd);
    printf("version size nonzero %d handle %lu\n", vs != 0, hnd);
    void *vi = malloc(vs + 16);
    E(GetFileVersionInfoA(path, 0, vs, vi));
    VS_FIXEDFILEINFO *ff;
    UINT len = 0;
    E(VerQueryValueA(vi, "\\", (void **)&ff, &len));
    printf("fixed len %u sig %lx ver %lx.%lx prod %lx.%lx mask %lx os %lx type %lx\n", len, ff->dwSignature, ff->dwFileVersionMS, ff->dwFileVersionLS, ff->dwProductVersionMS, ff->dwProductVersionLS, ff->dwFileFlagsMask, ff->dwFileOS, ff->dwFileType);
    DWORD *tr;
    E(VerQueryValueA(vi, "\\VarFileInfo\\Translation", (void **)&tr, &len));
    printf("translation len %u %lx\n", len, *tr);
    const char *keys[] = { "\\StringFileInfo\\041104b0\\CompanyName", "\\StringFileInfo\\041104b0\\FileDescription", "\\StringFileInfo\\041104B0\\ProductName", "\\StringFileInfo\\040904b0\\CompanyName", "\\StringFileInfo\\041104b0\\Missing" };
    for (int i = 0; i < 5; i++) {
        char *v = NULL;
        len = 99;
        SetLastError(0xdead);
        BOOL r = VerQueryValueA(vi, keys[i], (void **)&v, &len);
        printf("query %s: %d len %u [", keys[i], r, r ? len : 0);
        if (r) hex(v, strlen(v));
        printf("]\n");
    }
    WCHAR *wv;
    E(VerQueryValueW(vi, L"\\StringFileInfo\\041104b0\\FileDescription", (void **)&wv, &len));
    printf("wide len %u [", len); for (UINT i = 0; i < len; i++) printf("%04x", wv[i]); printf("]\n");
    E(GetFileVersionInfoSizeA("C:\\no\\such.exe", &hnd));
    return 0;
}
