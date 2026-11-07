/* Out-parameters that are pointers, handles or SIZE_Ts: 8 bytes on x64. sake once wrote only
 * their low half (Read/WriteProcessMemory's count, c2a5b74), which a program sees when its
 * variable was not zero before the call. Each variable starts as 0xcd bytes; "half" says that its
 * upper half (x64) or the whole of it (x86) was left as it was.
 * libs: -lshell32 -lole32 -loleaut32 -ladvapi32 -lrpcrt4 -lversion -lshlwapi -luuid */
#include "../probe.h"
#include <shlobj.h>
#include <objbase.h>
#include <wincrypt.h>
#include <rpc.h>
#include <sddl.h>

#define CANARY ((ULONG_PTR)0xcdcdcdcdcdcdcdcdull)
static int half(ULONG_PTR v) {
#ifdef _WIN64
    return (v >> 32) == 0xcdcdcdcdu;
#else
    return v == CANARY;
#endif
}
#define OUT(name, call, var)                                                                                                                                   \
    do {                                                                                                                                                       \
        var = (void *)CANARY;                                                                                                                                  \
        LONG_PTR r = (LONG_PTR)(call);                                                                                                                         \
        printf("%s: ret %s half %d null %d\n", name, r == 0 ? "0" : r == 1 ? "1" : "other", half((ULONG_PTR)(var)), (var) == 0);                               \
    } while (0)

static VOID CALLBACK wait_cb(PVOID p, BOOLEAN t) {}
static VOID CALLBACK timer_cb(PVOID p, BOOLEAN t) {}

int main(void) {
    void *p;
    HANDLE h;
    HKEY k;
    HMODULE m;
    SIZE_T sz;

    /* shell */
    PWSTR kp;
    OUT("SHGetKnownFolderPath", SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, NULL, &kp), kp);
    if (!half((ULONG_PTR)kp)) CoTaskMemFree(kp);
    PIDLIST_ABSOLUTE pidl;
    OUT("SHGetFolderLocation", SHGetFolderLocation(NULL, CSIDL_APPDATA, NULL, 0, &pidl), pidl);
    OUT("SHGetSpecialFolderLocation", SHGetSpecialFolderLocation(NULL, CSIDL_APPDATA, &pidl), pidl);
    IMalloc *ma;
    OUT("SHGetMalloc", SHGetMalloc(&ma), ma);
    OUT("CoGetMalloc", CoGetMalloc(1, &ma), ma);

    /* kernel32 */
    OUT("RegisterWaitForSingleObject", RegisterWaitForSingleObject(&h, CreateEventA(NULL, FALSE, FALSE, NULL), wait_cb, NULL, INFINITE, WT_EXECUTEONLYONCE), h);
    if (!half((ULONG_PTR)h)) UnregisterWait(h);
    OUT("CreateTimerQueueTimer", CreateTimerQueueTimer(&h, NULL, timer_cb, NULL, 100000, 0, 0), h);
    if (!half((ULONG_PTR)h)) DeleteTimerQueueTimer(NULL, h, NULL);
    HANDLE rd, wr;
    rd = (HANDLE)CANARY;
    OUT("CreatePipe write end", CreatePipe(&rd, &wr, NULL, 0), wr);
    printf("CreatePipe read end: half %d\n", half((ULONG_PTR)rd));
    OUT("DuplicateHandle", DuplicateHandle(GetCurrentProcess(), rd, GetCurrentProcess(), &h, 0, FALSE, DUPLICATE_SAME_ACCESS), h);
    OUT("GetModuleHandleExA", GetModuleHandleExA(0, "kernel32.dll", &m), m);
    OUT("GetModuleHandleExW from address", GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, (LPCWSTR)main, &m), m);
    char full[MAX_PATH], *part;
    OUT("GetFullPathNameA file part", GetFullPathNameA("x\\name.txt", MAX_PATH, full, &part) > 0, part);
    OUT("GetFullPathNameA dir file part", GetFullPathNameA("x\\", MAX_PATH, full, &part) > 0, part);
    OUT("SearchPathA file part", SearchPathA(NULL, "kernel32.dll", NULL, MAX_PATH, full, &part) > 0, part);
    char *msg;
    OUT("FormatMessageA ALLOCATE_BUFFER", FormatMessageA(FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS, NULL, 2, 0, (char *)&msg, 0, NULL) > 0, msg);
    if (!half((ULONG_PTR)msg)) LocalFree(msg);
    HANDLE port = CreateIoCompletionPort(INVALID_HANDLE_VALUE, NULL, 0, 1);
    PostQueuedCompletionStatus(port, 5, 77, NULL);
    DWORD bytes;
    ULONG_PTR key = CANARY;
    LPOVERLAPPED ov;
    OUT("GetQueuedCompletionStatus overlapped", GetQueuedCompletionStatus(port, &bytes, &key, &ov, 1000), ov);
    printf("GetQueuedCompletionStatus key: half %d value %d\n", half(key), (int)(key & 0xffff));
    CloseHandle(port);
    INIT_ONCE once = INIT_ONCE_STATIC_INIT;
    BOOL pending;
    OUT("InitOnceBeginInitialize context", InitOnceBeginInitialize(&once, 0, &pending, &p), p);
    InitOnceComplete(&once, 0, (void *)0x1230);
    OUT("InitOnceBeginInitialize done context", InitOnceBeginInitialize(&once, INIT_ONCE_CHECK_ONLY, &pending, &p), p);
    printf("context value %lx\n", (unsigned long)(ULONG_PTR)p);
    OUT("OpenProcessToken", OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &h), h);
    {
        char d[64];
        SIZE_T n = CANARY;
        ReadProcessMemory(GetCurrentProcess(), "abc", d, 3, &n);
        printf("ReadProcessMemory count: half %d value %d\n", half(n), (int)(n & 0xff));
    }

    /* registry */
    OUT("RegOpenKeyExA", RegOpenKeyExA(HKEY_CURRENT_USER, "Software", 0, KEY_READ, &k) == 0, k);
    OUT("RegCreateKeyExA", RegCreateKeyExA(HKEY_CURRENT_USER, "Software\\sake-winref-outptr", 0, NULL, 0, KEY_ALL_ACCESS, NULL, &k, NULL) == 0, k);
    RegCloseKey(k);
    RegDeleteKeyA(HKEY_CURRENT_USER, "Software\\sake-winref-outptr");
    OUT("RegOpenKeyA", RegOpenKeyA(HKEY_CURRENT_USER, "Software", &k) == 0, k);

    /* crypto */
    HCRYPTPROV prov;
    OUT("CryptAcquireContextA", CryptAcquireContextA(&prov, NULL, NULL, PROV_RSA_FULL, CRYPT_VERIFYCONTEXT), *(void **)&prov);
    HCRYPTHASH hh;
    OUT("CryptCreateHash", CryptCreateHash(prov, CALG_MD5, 0, 0, &hh), *(void **)&hh);
    CryptHashData(hh, (BYTE *)"k", 1, 0);
    HCRYPTKEY key2;
    OUT("CryptDeriveKey", CryptDeriveKey(prov, CALG_RC4, hh, 0, &key2), *(void **)&key2);

    /* rpcrt4, ole */
    UUID u = { 0x6b29fc40, 0xca47, 0x1067, { 0xb3, 0x1d, 0, 0xdd, 1, 6, 0x62, 0xda } };
    RPC_CSTR us;
    OUT("UuidToStringA", UuidToStringA(&u, &us) == 0, us);
    if (!half((ULONG_PTR)us)) {
        RpcStringFreeA(&us);
        printf("RpcStringFreeA: null %d\n", us == NULL);
    }
    LPOLESTR os;
    OUT("StringFromCLSID", StringFromCLSID(&u, &os) == 0, os);
    IStream *st;
    OUT("CreateStreamOnHGlobal", CreateStreamOnHGlobal(NULL, TRUE, &st) == 0, st);
    if (!half((ULONG_PTR)st)) {
        HGLOBAL g = (HGLOBAL)CANARY;
        GetHGlobalFromStream(st, &g);
        printf("GetHGlobalFromStream: half %d\n", half((ULONG_PTR)g));
    }
    SAFEARRAY *sa = SafeArrayCreateVector(VT_UI1, 0, 16);
    OUT("SafeArrayAccessData", SafeArrayAccessData(sa, &p) == 0, p);
    SafeArrayUnaccessData(sa);
    BSTR b = SysAllocString(L"x");
    OUT("SysAllocString", (b = SysAllocString(L"x")) != NULL, b);

    /* version */
    char exe[MAX_PATH];
    GetSystemDirectoryA(exe, MAX_PATH);
    strcat(exe, "\\kernel32.dll");
    DWORD vsize = GetFileVersionInfoSizeA(exe, NULL);
    void *vi = malloc(vsize ? vsize : 1);
    GetFileVersionInfoA(exe, 0, vsize, vi);
    UINT vl;
    OUT("VerQueryValueA root", VerQueryValueA(vi, "\\", &p, &vl), p);
    OUT("VerQueryValueA translation", VerQueryValueA(vi, "\\VarFileInfo\\Translation", &p, &vl), p);

    /* security */
    PSID sid;
    SID_IDENTIFIER_AUTHORITY nt = SECURITY_NT_AUTHORITY;
    OUT("AllocateAndInitializeSid", AllocateAndInitializeSid(&nt, 2, SECURITY_BUILTIN_DOMAIN_RID, DOMAIN_ALIAS_RID_ADMINS, 0, 0, 0, 0, 0, 0, &sid), sid);
    char *ss;
    OUT("ConvertSidToStringSidA", ConvertSidToStringSidA(sid, &ss), ss);
    if (!half((ULONG_PTR)ss)) printf("sid %s\n", ss);
    PSID sid2;
    OUT("ConvertStringSidToSidA", ConvertStringSidToSidA("S-1-5-32-544", &sid2), sid2);
    (void)sz;
    return 0;
}
