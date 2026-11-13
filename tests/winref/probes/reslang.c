/* Which language FindResource / FindResourceEx(LANG_NEUTRAL) / LoadString take when a module
 * has the same resource in several languages (probes/reslang.rc): the UI language (Japanese,
 * set for the thread: the runner's is en-US), else English (US), else the first; a
 * LANG_NEUTRAL entry before all. sake took the first one, so 虜ノ焔 体験版's launcher (string
 * tables in 0x404, 0x409, 0x411, 0x804) showed its buttons in Chinese.
 * libs: */
#include "../probe.h"

static void res(const char *name) {
    HRSRC r = FindResourceA(NULL, name, (LPCSTR)RT_RCDATA);
    printf("FindResource %s: %s\n", name, r ? (const char *)LockResource(LoadResource(NULL, r)) : "none");
    r = FindResourceExA(NULL, (LPCSTR)RT_RCDATA, name, MAKELANGID(LANG_NEUTRAL, SUBLANG_NEUTRAL));
    printf("FindResourceEx neutral %s: %s\n", name, r ? (const char *)LockResource(LoadResource(NULL, r)) : "none");
}

int main(void) {
    SetThreadUILanguage(MAKELANGID(LANG_JAPANESE, SUBLANG_DEFAULT));
    res("ALL4");
    res("NOJA");
    res("ZHONLY");
    res("NEUTRAL");
    for (int id = 100; id <= 300; id += 100) {
        char a[64] = "";
        WCHAR w[64] = L"";
        int na = LoadStringA(NULL, id, a, sizeof a), nw = LoadStringW(NULL, id, w, 64);
        printf("LoadString %d: %d %s / %d", id, na, a, nw);
        for (int i = 0; i < nw; i++) printf(" %04x", w[i]);
        printf("\n");
    }
    return 0;
}
