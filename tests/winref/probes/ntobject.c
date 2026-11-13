/* NtQueryObject: the NT name of a file handle (ObjectNameInformation, "\Device\HarddiskVolumeN\..."),
 * of a directory and a drive's root, an unnamed object's empty name, the type names (ObjectType-
 * Information), ObjectBasicInformation's size, and the statuses of a short buffer, a bad handle
 * and an unknown class. Enigma Virtual Box's NtCreateFile hook asks it for the name of the root
 * directory handle kernel32 passes with a relative path (ane_ane_trial.exe called a null pointer
 * while sake had no NtQueryObject). Printed so that it does not depend on where the probe runs. */
#include "../probe.h"

typedef struct { USHORT Length, MaximumLength; PWSTR Buffer; } US;
typedef LONG (WINAPI *QUERY)(HANDLE, int, void *, ULONG, ULONG *);
static QUERY query;
static union { US s; ULONG_PTR align; unsigned char b[4096]; } u;

/* the type size of OBJECT_TYPE_INFORMATION: the name after it */
#define TYPE_SIZE (sizeof(void *) == 8 ? 0x68 : 0x60)

static void name_of(const char *what, HANDLE h, const WCHAR *tail) {
    ULONG len = 0xdead;
    memset(&u, 0xcc, sizeof u);
    LONG st = query(h, 1, &u, 0, &len);
    ULONG need = len;
    printf("%s: empty buffer %08lx need>=8 %d\n", what, (unsigned long)st, need >= sizeof(US));
    len = 0xdead;
    st = query(h, 1, &u, sizeof u, &len);
    printf("%s: %08lx len==need %d\n", what, (unsigned long)st, len == need);
    if (st) return;
    size_t n = u.s.Length / 2;
    WCHAR full[MAX_PATH] = L"";
    GetFullPathNameW(tail, MAX_PATH, full, NULL);
    /* the rest of the Win32 path after "X:" */
    const WCHAR *rest = full[1] == L':' ? full + 2 : full;
    size_t k = wcslen(rest);
    int device = n >= 8 && !_wcsnicmp(u.s.Buffer, L"\\Device\\", 8);
    int ends = n >= k && !_wcsnicmp(u.s.Buffer + n - k, rest, k);
    printf("%s: device %d ends with the path %d trailing backslash %d max %d buffer after %d nul %d len %d\n", what, device, ends,
           n && u.s.Buffer[n - 1] == L'\\', u.s.MaximumLength - u.s.Length, (int)((unsigned char *)u.s.Buffer - u.b),
           u.s.Buffer[n] == 0, len == sizeof(US) + u.s.MaximumLength);
    /* one byte short of the whole */
    len = 0xdead;
    st = query(h, 1, &u, need - 1, &len);
    printf("%s: short %08lx len==need %d\n", what, (unsigned long)st, len == need);
}

static void type_of(const char *what, HANDLE h) {
    ULONG len = 0xdead;
    memset(&u, 0xcc, sizeof u);
    LONG st = query(h, 2, &u, sizeof u, &len);
    if (st) {
        printf("%s type: %08lx\n", what, (unsigned long)st);
        return;
    }
    printf("%s type: %.*ls len-size>=max %d max-len %d buffer after %d\n", what, u.s.Length / 2, u.s.Buffer, len - TYPE_SIZE >= u.s.MaximumLength,
           u.s.MaximumLength - u.s.Length, (int)((unsigned char *)u.s.Buffer - u.b));
    len = 0xdead;
    st = query(h, 2, &u, TYPE_SIZE, &len);
    printf("%s type short: %08lx len>size %d\n", what, (unsigned long)st, len > TYPE_SIZE);
}

int main(void) {
    query = (QUERY)GetProcAddress(GetModuleHandleA("ntdll.dll"), "NtQueryObject");
    printf("NtQueryObject %d ZwQueryObject %d\n", query != NULL, GetProcAddress(GetModuleHandleA("ntdll.dll"), "ZwQueryObject") != NULL);
    if (!query) return 1;
    HANDLE f = CreateFileW(L"ntobject.dat", GENERIC_READ | GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_FLAG_DELETE_ON_CLOSE, NULL);
    CreateDirectoryW(L"ntobject.dir", NULL);
    HANDLE d = CreateFileW(L"ntobject.dir", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, NULL);
    HANDLE root = CreateFileW(L"\\", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, NULL);
    HANDLE ev = CreateEventW(NULL, TRUE, FALSE, NULL);
    printf("handles %d %d %d %d\n", f != INVALID_HANDLE_VALUE, d != INVALID_HANDLE_VALUE, root != INVALID_HANDLE_VALUE, ev != NULL);
    name_of("file", f, L"ntobject.dat");
    name_of("dir", d, L"ntobject.dir");
    name_of("root", root, L"\\");
    ULONG len = 0xdead;
    memset(&u, 0xcc, sizeof u);
    LONG st = query(ev, 1, &u, sizeof u, &len);
    printf("event name: %08lx len %lu length %u max %u buffer null %d\n", (unsigned long)st, (unsigned long)len, u.s.Length, u.s.MaximumLength, u.s.Buffer == NULL);
    type_of("file", f);
    type_of("dir", d);
    type_of("event", ev);
    len = 0xdead;
    st = query(f, 0, &u, sizeof u, &len);
    printf("basic: %08lx len %lu\n", (unsigned long)st, (unsigned long)len);
    len = 0xdead;
    st = query(f, 0, &u, 8, &len);
    printf("basic short: %08lx len %lu\n", (unsigned long)st, (unsigned long)len);
    st = query((HANDLE)(ULONG_PTR)0x1230, 1, &u, sizeof u, &len);
    printf("bad handle: %08lx\n", (unsigned long)st);
    st = query(f, 99, &u, sizeof u, &len);
    printf("class 99: %08lx\n", (unsigned long)st);
    CloseHandle(ev);
    CloseHandle(root);
    CloseHandle(d);
    CloseHandle(f);
    RemoveDirectoryW(L"ntobject.dir");
    return 0;
}
