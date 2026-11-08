/* kernel32's file functions go through ntdll's, so a program that patches ntdll's code sees them
 * there: Enigma Virtual Box puts a jump on ZwCreateFile, ZwReadFile, ZwClose... and serves the
 * files packed into the exe from its hooks (b2019_anim, #21). This probe hooks the same functions
 * the same way (5 bytes: a jump; the original 5 bytes and a jump back for what it does not
 * serve) and serves one file, "nthook-virtual.dat", that is not on the disk; it prints what
 * kernel32 asked ntdll for (the call, its class and flags) and what kernel32 gave back.
 * Nt* and Zw* are one function in ntdll. arch: 32 */
#include "../probe.h"

typedef struct { USHORT Length, MaximumLength; PWSTR Buffer; } US;
typedef struct { ULONG Length; HANDLE Root; US *Name; ULONG Attributes; void *Sd, *Qos; } OA;
typedef struct { union { LONG Status; void *p; }; ULONG_PTR Information; } IOSB;

#define VH ((HANDLE)(ULONG_PTR)0x7ff4)
static const char data[] = "hello, virtual world";
#define LEN ((LONGLONG)sizeof data - 1)
static LONGLONG pos;
static int opened;

static char logbuf[4096];
static void say(const char *f, ...) {
    va_list a;
    va_start(a, f);
    size_t n = strlen(logbuf);
    vsnprintf(logbuf + n, sizeof logbuf - n, f, a);
    va_end(a);
}
static void flush(const char *what) {
    printf("%s:%s\n", what, logbuf);
    logbuf[0] = 0;
}

/* whether an NT name is \??\ and the full path of nthook-virtual.dat */
static int ours(OA *oa) {
    static const WCHAR tail[] = L"\\nthook-virtual.dat";
    size_t n = oa->Name->Length / 2, k = wcslen(tail);
    if (n < k || _wcsnicmp(oa->Name->Buffer + n - k, tail, k)) return 0;
    say(" name %s%s root %d attrs %#lx", wcsncmp(oa->Name->Buffer, L"\\??\\", 4) ? "other" : "\\??\\", oa->Name->Buffer[5] == ':' ? "X:" : "?", oa->Root != 0, oa->Attributes);
    return 1;
}

typedef LONG (WINAPI *CREATE)(HANDLE *, ULONG, OA *, IOSB *, LARGE_INTEGER *, ULONG, ULONG, ULONG, ULONG, void *, ULONG);
typedef LONG (WINAPI *OPEN)(HANDLE *, ULONG, OA *, IOSB *, ULONG, ULONG);
typedef LONG (WINAPI *READ)(HANDLE, HANDLE, void *, void *, IOSB *, void *, ULONG, LARGE_INTEGER *, ULONG *);
typedef LONG (WINAPI *QINFO)(HANDLE, IOSB *, void *, ULONG, int);
typedef LONG (WINAPI *CLOSE)(HANDLE);
typedef LONG (WINAPI *QATTR)(OA *, void *);
static CREATE create0;
static OPEN open0;
static READ read0;
static QINFO query0, set0;
static CLOSE close0;
static QATTR attrs0, full0;

static LONG WINAPI create(HANDLE *h, ULONG access, OA *oa, IOSB *io, LARGE_INTEGER *alloc, ULONG attrs, ULONG share, ULONG disp, ULONG opts, void *ea, ULONG ealen) {
    if (!ours(oa)) return create0(h, access, oa, io, alloc, attrs, share, disp, opts, ea, ealen);
    say(" NtCreateFile access %#lx file attrs %#lx share %lu disposition %lu options %#lx alloc %d ea %d", access, attrs, share, disp, opts, alloc != 0, ea != 0);
    if (disp == 2 && opened) { io->Status = 0; return 0xc0000035; }
    *h = VH;
    pos = 0;
    io->Status = 0;
    io->Information = opened ? 1 : 2;
    opened = 1;
    return 0;
}
static LONG WINAPI open_(HANDLE *h, ULONG access, OA *oa, IOSB *io, ULONG share, ULONG opts) {
    if (!ours(oa)) return open0(h, access, oa, io, share, opts);
    say(" NtOpenFile access %#lx share %lu options %#lx", access, share, opts);
    *h = VH;
    pos = 0;
    io->Status = 0;
    io->Information = 1;
    return 0;
}
static LONG WINAPI read_(HANDLE h, HANDLE ev, void *apc, void *ctx, IOSB *io, void *buf, ULONG n, LARGE_INTEGER *off, ULONG *key) {
    if (h != VH) return read0(h, ev, apc, ctx, io, buf, n, off, key);
    say(" NtReadFile %lu event %d apc %d ctx %d offset %s key %d", n, ev != 0, apc != 0, ctx != 0, off ? "given" : "none", key != 0);
    if (off) {
        say(" %lld", off->QuadPart);
        if (off->QuadPart >= 0) pos = off->QuadPart;
    }
    if (pos >= LEN && n) { io->Status = 0xc0000011; io->Information = 0; return 0xc0000011; }
    ULONG k = (ULONG)(LEN - pos < n ? LEN - pos : n);
    memcpy(buf, data + pos, k);
    pos += k;
    io->Status = 0;
    io->Information = k;
    if (ev) SetEvent(ev);
    return 0;
}
static LONG WINAPI query(HANDLE h, IOSB *io, void *buf, ULONG len, int cls) {
    if (h != VH) return query0(h, io, buf, len, cls);
    say(" NtQueryInformationFile class %d len %lu", cls, len);
    memset(buf, 0, len);
    if (cls == 5 && len >= 24) {
        ((LONGLONG *)buf)[0] = 4096;
        ((LONGLONG *)buf)[1] = LEN;
        ((ULONG *)buf)[4] = 1;
        io->Information = 24;
    } else if (cls == 14 && len >= 8) {
        *(LONGLONG *)buf = pos;
        io->Information = 8;
    } else {
        io->Status = 0xc0000003;
        return 0xc0000003;
    }
    io->Status = 0;
    return 0;
}
static LONG WINAPI set_(HANDLE h, IOSB *io, void *buf, ULONG len, int cls) {
    if (h != VH) return set0(h, io, buf, len, cls);
    say(" NtSetInformationFile class %d len %lu", cls, len);
    if (cls == 14) {
        say(" to %lld", *(LONGLONG *)buf);
        pos = *(LONGLONG *)buf;
    }
    io->Status = 0;
    io->Information = 0;
    return 0;
}
static LONG WINAPI close_(HANDLE h) {
    if (h != VH) return close0(h);
    say(" NtClose");
    return 0;
}
static LONG WINAPI attrs_(OA *oa, void *out) {
    if (!ours(oa)) return attrs0(oa, out);
    say(" NtQueryAttributesFile");
    memset(out, 0, 40);
    ((ULONG *)out)[8] = FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_ARCHIVE;
    ((LONGLONG *)out)[0] = 0x01d0000000000001LL;
    ((LONGLONG *)out)[1] = 0x01d0000000000002LL;
    ((LONGLONG *)out)[2] = 0x01d0000000000003LL;
    ((LONGLONG *)out)[3] = 0x01d0000000000004LL;
    return 0;
}
static LONG WINAPI full_(OA *oa, void *out) {
    if (!ours(oa)) return full0(oa, out);
    say(" NtQueryFullAttributesFile");
    memset(out, 0, 56);
    ((LONGLONG *)out)[0] = 0x01d0000000000001LL;
    ((LONGLONG *)out)[1] = 0x01d0000000000002LL;
    ((LONGLONG *)out)[2] = 0x01d0000000000003LL;
    ((LONGLONG *)out)[3] = 0x01d0000000000004LL;
    ((LONGLONG *)out)[4] = 4096;
    ((LONGLONG *)out)[5] = LEN;
    ((ULONG *)out)[12] = FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_ARCHIVE;
    return 0;
}

/* a jump to `to` on the first 5 bytes of ntdll's `name`; returns the original (those 5 bytes
 * and a jump back) */
static void *hook(const char *name, void *to) {
    static unsigned char *tramp;
    if (!tramp) tramp = VirtualAlloc(NULL, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
    unsigned char *f = (unsigned char *)GetProcAddress(GetModuleHandleA("ntdll.dll"), name);
    unsigned char *t = tramp;
    DWORD old;
    tramp += 16;
    memcpy(t, f, 5);
    t[5] = 0xe9;
    *(LONG *)(t + 6) = (LONG)(f + 5 - (t + 10));
    VirtualProtect(f, 5, PAGE_EXECUTE_READWRITE, &old);
    f[0] = 0xe9;
    *(LONG *)(f + 1) = (LONG)((unsigned char *)to - (f + 5));
    VirtualProtect(f, 5, old, &old);
    FlushInstructionCache(GetCurrentProcess(), f, 5);
    return t;
}

int main(void) {
    HMODULE nt = GetModuleHandleA("ntdll.dll");
    static const char *same[] = { "Close", "CreateFile", "OpenFile", "ReadFile", "WriteFile", "QueryInformationFile", "SetInformationFile", "QueryAttributesFile", "QueryFullAttributesFile", "QueryDirectoryFile", "ProtectVirtualMemory", "QueryInformationProcess" };
    for (int i = 0; i < (int)(sizeof same / sizeof *same); i++) {
        char a[64], b[64];
        sprintf(a, "Nt%s", same[i]);
        sprintf(b, "Zw%s", same[i]);
        printf("%s and %s: %s\n", a, b, GetProcAddress(nt, a) == GetProcAddress(nt, b) ? "one function" : "two");
    }
    /* (as Enigma Virtual Box: the Zw names) */
    create0 = hook("ZwCreateFile", create);
    open0 = hook("ZwOpenFile", open_);
    read0 = hook("ZwReadFile", read_);
    query0 = hook("ZwQueryInformationFile", query);
    set0 = hook("ZwSetInformationFile", set_);
    close0 = hook("ZwClose", close_);
    attrs0 = hook("ZwQueryAttributesFile", attrs_);
    full0 = hook("ZwQueryFullAttributesFile", full_);

    char buf[64];
    DWORD n = 0xdead, r;
    LONG hi;
    SetLastError(0xdead);
    HANDLE h = CreateFileA("nthook-virtual.dat", GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
    say(" -> %s error %lu", h == VH ? "the hook's handle" : h == INVALID_HANDLE_VALUE ? "INVALID_HANDLE_VALUE" : "another handle", GetLastError());
    flush("CreateFileA OPEN_EXISTING");
    r = GetFileSize(h, &n);
    say(" -> %lu high %lu", r, n);
    flush("GetFileSize");
    LARGE_INTEGER li = { 0 };
    r = GetFileSizeEx(h, &li);
    say(" -> %lu %lld", r, li.QuadPart);
    flush("GetFileSizeEx");
    memset(buf, 0, sizeof buf);
    r = ReadFile(h, buf, 5, &n, NULL);
    say(" -> %lu read %lu \"%s\"", r, n, buf);
    flush("ReadFile 5");
    r = SetFilePointer(h, 7, NULL, FILE_BEGIN);
    say(" -> %lu", r);
    flush("SetFilePointer 7 FILE_BEGIN");
    r = SetFilePointer(h, 0, NULL, FILE_CURRENT);
    say(" -> %lu", r);
    flush("SetFilePointer 0 FILE_CURRENT");
    hi = -1;
    r = SetFilePointer(h, -5, &hi, FILE_END);
    say(" -> %lu high %ld", r, hi);
    flush("SetFilePointer -5 FILE_END");
    memset(buf, 0, sizeof buf);
    r = ReadFile(h, buf, 40, &n, NULL);
    say(" -> %lu read %lu \"%s\"", r, n, buf);
    flush("ReadFile 40");
    n = 0xdead;
    r = ReadFile(h, buf, 40, &n, NULL);
    say(" -> %lu read %lu", r, n);
    flush("ReadFile at the end");
    SetLastError(0xdead);
    r = SetFilePointer(h, -100, NULL, FILE_CURRENT);
    say(" -> %#lx error %lu", r, GetLastError());
    flush("SetFilePointer -100 FILE_CURRENT");
    LARGE_INTEGER to = { .QuadPart = 2 }, now = { 0 };
    r = SetFilePointerEx(h, to, &now, FILE_BEGIN);
    say(" -> %lu %lld", r, now.QuadPart);
    flush("SetFilePointerEx 2 FILE_BEGIN");
    OVERLAPPED ov = { 0 };
    ov.Offset = 14;
    ov.hEvent = CreateEventA(NULL, TRUE, FALSE, NULL);
    memset(buf, 0, sizeof buf);
    n = 0xdead;
    r = ReadFile(h, buf, 3, &n, &ov);
    say(" -> %lu read %lu \"%s\" Internal %#lx InternalHigh %lu", r, n, buf, (ULONG)ov.Internal, (ULONG)ov.InternalHigh);
    flush("ReadFile 3 at OVERLAPPED 14");
    CloseHandle(ov.hEvent);
    logbuf[0] = 0;
    r = CloseHandle(h);
    say(" -> %lu", r);
    flush("CloseHandle");

    SetLastError(0xdead);
    h = CreateFileW(L"nthook-virtual.dat", GENERIC_READ | GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_HIDDEN | FILE_FLAG_SEQUENTIAL_SCAN, NULL);
    say(" -> %s error %lu", h == VH ? "the hook's handle" : "other", GetLastError());
    flush("CreateFileW CREATE_ALWAYS");
    CloseHandle(h);
    logbuf[0] = 0;
    SetLastError(0xdead);
    h = CreateFileA("nthook-virtual.dat", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_DELETE, NULL, CREATE_NEW, FILE_FLAG_OVERLAPPED | FILE_FLAG_RANDOM_ACCESS, NULL);
    say(" -> %s error %lu", h == INVALID_HANDLE_VALUE ? "INVALID_HANDLE_VALUE" : "a handle", GetLastError());
    flush("CreateFileA CREATE_NEW");
    SetLastError(0xdead);
    h = CreateFileA("nthook-virtual.dat", GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_ALWAYS, FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_WRITE_THROUGH | FILE_FLAG_NO_BUFFERING, NULL);
    say(" -> %s error %lu", h == VH ? "the hook's handle" : "other", GetLastError());
    flush("CreateFileA OPEN_ALWAYS");
    CloseHandle(h);
    logbuf[0] = 0;

    SetLastError(0xdead);
    r = GetFileAttributesA("nthook-virtual.dat");
    say(" -> %#lx error %lu", r, GetLastError());
    flush("GetFileAttributesA");
    r = GetFileAttributesW(L"nthook-virtual.dat");
    say(" -> %#lx", r);
    flush("GetFileAttributesW");
    WIN32_FILE_ATTRIBUTE_DATA ad;
    memset(&ad, 0xcc, sizeof ad);
    r = GetFileAttributesExA("nthook-virtual.dat", GetFileExInfoStandard, &ad);
    say(" -> %lu attrs %#lx created %#lx%08lx accessed %#lx%08lx written %#lx%08lx size %lu %lu", r, ad.dwFileAttributes,
        ad.ftCreationTime.dwHighDateTime, ad.ftCreationTime.dwLowDateTime, ad.ftLastAccessTime.dwHighDateTime, ad.ftLastAccessTime.dwLowDateTime,
        ad.ftLastWriteTime.dwHighDateTime, ad.ftLastWriteTime.dwLowDateTime, ad.nFileSizeHigh, ad.nFileSizeLow);
    flush("GetFileAttributesExA");

    /* a file on the disk goes on to ntdll's own code */
    h = CreateFileA("nthook-real.tmp", GENERIC_READ | GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD w = 0;
    BOOL ok = WriteFile(h, "abcdef", 6, &w, NULL);
    SetFilePointer(h, 2, NULL, FILE_BEGIN);
    memset(buf, 0, sizeof buf);
    ReadFile(h, buf, 10, &n, NULL);
    DWORD size = GetFileSize(h, NULL);
    printf("real file: opened %d wrote %d %lu size %lu read %lu \"%s\"\n", h != INVALID_HANDLE_VALUE, ok, w, size, n, buf);
    BOOL closed = CloseHandle(h);
    r = GetFileAttributesA("nthook-real.tmp");
    printf("real file: closed %d attrs %#lx deleted %d\n", closed, r, DeleteFileA("nthook-real.tmp"));
    SetLastError(0xdead);
    r = GetFileAttributesA("nthook-missing.dat");
    printf("missing file: %#lx error %lu\n", r, GetLastError());
    SetLastError(0xdead);
    h = CreateFileA("nthook-missing\\x.dat", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL);
    printf("missing folder: %s error %lu\n", h == INVALID_HANDLE_VALUE ? "INVALID_HANDLE_VALUE" : "a handle", GetLastError());
    printf("log left: \"%s\"\n", logbuf);
    return 0;
}
