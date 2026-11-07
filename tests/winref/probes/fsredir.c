/* File system redirection switches: ntdll's RtlWow64EnableFsRedirection(Ex) (the value kept
 * per thread, any value given back as set, bad pointers) and kernel32's
 * Wow64DisableWow64FsRedirection / Wow64RevertWow64FsRedirection / Wow64EnableWow64FsRedirection
 * (what they return and their errors; -32 runs under WoW64 on the runner, -64 natively). */
#include "../probe.h"

typedef LONG(WINAPI *rtl_ex_t)(ULONG, ULONG *);
typedef LONG(WINAPI *rtl_t)(BOOLEAN);
typedef BOOL(WINAPI *dis_t)(void **);
typedef BOOL(WINAPI *rev_t)(void *);
typedef BOOLEAN(WINAPI *en_t)(BOOLEAN);

static rtl_ex_t ex;

static void step(const char *what, ULONG v) {
    ULONG cur = 0xdeadbeef;
    LONG s = ex(v, &cur);
    printf("%s: Ex(%#lx) status %#lx old %#lx\n", what, v, s, cur);
}

static DWORD WINAPI other(void *p) {
    (void)p;
    step("other thread", 0);
    return 0;
}

int main(void) {
    HMODULE nt = GetModuleHandleA("ntdll.dll"), k = GetModuleHandleA("kernel32.dll");
    ex = (rtl_ex_t)GetProcAddress(nt, "RtlWow64EnableFsRedirectionEx");
    rtl_t en = (rtl_t)GetProcAddress(nt, "RtlWow64EnableFsRedirection");
    dis_t dis = (dis_t)GetProcAddress(k, "Wow64DisableWow64FsRedirection");
    rev_t rev = (rev_t)GetProcAddress(k, "Wow64RevertWow64FsRedirection");
    en_t ken = (en_t)GetProcAddress(k, "Wow64EnableWow64FsRedirection");
    printf("exports: Ex %d Enable %d Disable %d Revert %d k32 Enable %d\n", !!ex, !!en, !!dis, !!rev, !!ken);
    if (!ex || !en) return 0;

    step("first", 0);
    step("set 1", 1);
    step("again 1", 1);
    printf("Enable(TRUE) %#lx\n", en(TRUE));
    step("after Enable(TRUE)", 1);
    step("123", 123);
    step("deadbeef", 0xdeadbeef);
    printf("Enable(FALSE) %#lx\n", en(FALSE));
    step("after Enable(FALSE)", 0);
    for (ULONG_PTR p = 0; p < 2; p++) printf("Ex(1, %#lx) status %#lx\n", (unsigned long)p, ex(1, (ULONG *)p));
    printf("Ex(1, 0xdeadbeef) status %#lx\n", ex(1, (ULONG *)(ULONG_PTR)0xdeadbeef));
    step("after bad pointers", 0);

    HANDLE t = CreateThread(NULL, 0, other, NULL, 0, NULL);
    WaitForSingleObject(t, INFINITE);
    CloseHandle(t);
    step("this thread", 0);

    if (dis && rev) {
        void *old = (void *)0x1234;
        SetLastError(0xdeadbeef);
        BOOL r = dis(&old);
        printf("Disable %d err %lu old %p\n", r, GetLastError(), old);
        step("after Disable", 0);
        SetLastError(0xdeadbeef);
        r = rev(old);
        printf("Revert %d err %lu\n", r, GetLastError());
        step("after Revert", 0);
        SetLastError(0xdeadbeef);
        r = dis(NULL);
        printf("Disable(NULL) %d err %lu\n", r, GetLastError());
        SetLastError(0xdeadbeef);
        r = rev((void *)5);
        printf("Revert(5) %d err %lu\n", r, GetLastError());
        step("after Revert(5)", 0);
    }
    if (ken) {
        SetLastError(0xdeadbeef);
        BOOLEAN b = ken(FALSE);
        printf("k32 Enable(FALSE) %d err %lu\n", b, GetLastError());
        step("after k32 Enable(FALSE)", 0);
    }
    return 0;
}
