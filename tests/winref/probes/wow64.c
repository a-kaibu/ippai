/* 64-bit Windows as a 32-bit process (WoW64, the -32 build) and a 64-bit one (-64) see it:
 * IsWow64Process(2), GetNativeSystemInfo against GetSystemInfo, GetSystemWow64Directory(2) and
 * its sizes, the Program Files / PROCESSOR_ARCHITECTURE variables, the shell's system and
 * Program Files folders (CSIDL and known folders), the registry's two views of
 * ProgramFilesDir and Wow6432Node, SysWOW64 / Sysnative in the file system and
 * NtQueryInformationProcess(ProcessWow64Information). NSIS's x64.nsh (#183) and Hime's Flash
 * loader (#209) ask these. Paths are printed in lower case (the runner's C:\Windows).
 * libs: -lshell32 -lole32 -ladvapi32 */
#include "../probe.h"
#include <shlobj.h>

static void lower(char *s) {
    for (; *s; s++)
        if (*s >= 'A' && *s <= 'Z') *s += 32;
}

static void env(const char *n) {
    char b[MAX_PATH];
    DWORD r = GetEnvironmentVariableA(n, b, sizeof b);
    if (!r) strcpy(b, "(none)");
    lower(b);
    printf("env %s: %s\n", n, b);
}

static void csidl(int id) {
    char b[MAX_PATH] = "";
    HRESULT hr = SHGetFolderPathA(NULL, id, NULL, 0, b);
    lower(b);
    printf("SHGetFolderPath %#x: %#lx %s\n", id, (unsigned long)hr, b);
}

static void known(const char *what, const GUID *id) {
    WCHAR *p = NULL;
    HRESULT hr = SHGetKnownFolderPath(id, 0, NULL, &p);
    char b[MAX_PATH] = "";
    if (SUCCEEDED(hr)) WideCharToMultiByte(CP_ACP, 0, p, -1, b, sizeof b, NULL, NULL);
    CoTaskMemFree(p);
    lower(b);
    printf("SHGetKnownFolderPath %s: %#lx %s\n", what, (unsigned long)hr, b);
}

static void regval(const char *what, REGSAM sam, const char *name) {
    HKEY k;
    LONG r = RegOpenKeyExA(HKEY_LOCAL_MACHINE, "SOFTWARE\\Microsoft\\Windows\\CurrentVersion", 0, KEY_READ | sam, &k);
    char b[MAX_PATH] = "";
    DWORD n = sizeof b, ty = 0;
    if (!r) {
        r = RegQueryValueExA(k, name, NULL, &ty, (BYTE *)b, &n);
        RegCloseKey(k);
    }
    lower(b);
    printf("registry %s %s: %ld type %lu %s\n", what, name, r, ty, b);
}

static void attrs(const char *path) {
    SetLastError(0xdeadbeef);
    DWORD a = GetFileAttributesA(path);
    printf("GetFileAttributes %s: %s", path, a == INVALID_FILE_ATTRIBUTES ? "none" : (a & FILE_ATTRIBUTE_DIRECTORY) ? "directory" : "file");
    if (a == INVALID_FILE_ATTRIBUTES) printf(" err %lu", GetLastError());
    printf("\n");
}

typedef BOOL(WINAPI *wow2_t)(HANDLE, USHORT *, USHORT *);
typedef UINT(WINAPI *dir2a_t)(char *, UINT, WORD);
typedef LONG(WINAPI *nqip_t)(HANDLE, ULONG, void *, ULONG, ULONG *);

int main(void) {
    BOOL w = 2;
    SetLastError(0xdeadbeef);
    BOOL r = IsWow64Process(GetCurrentProcess(), &w);
    printf("IsWow64Process(self) %d wow64 %d err %lu\n", r, w, r ? 0 : GetLastError());
    w = 2;
    SetLastError(0xdeadbeef);
    r = IsWow64Process((HANDLE)(ULONG_PTR)0x1234, &w);
    printf("IsWow64Process(0x1234) %d wow64 %d err %lu\n", r, w, GetLastError());
    HMODULE k32 = GetModuleHandleA("kernel32.dll"), kb = GetModuleHandleA("kernelbase.dll");
    wow2_t wow2 = (wow2_t)GetProcAddress(k32, "IsWow64Process2");
    if (wow2) {
        USHORT pm = 0xcccc, nm = 0xcccc;
        r = wow2(GetCurrentProcess(), &pm, &nm);
        printf("IsWow64Process2 %d process %#x native %#x\n", r, pm, nm);
    }

    SYSTEM_INFO a, b;
    memset(&a, 0xcc, sizeof a);
    memset(&b, 0xcc, sizeof b);
    GetSystemInfo(&a);
    GetNativeSystemInfo(&b);
    printf("GetSystemInfo architecture %u type %lu\n", a.wProcessorArchitecture, a.dwProcessorType);
    printf("GetNativeSystemInfo architecture %u type %lu\n", b.wProcessorArchitecture, b.dwProcessorType);
    printf("native: page==%d granularity==%d min==%d max==%d mask==%d processors==%d level==%d revision==%d\n",
           a.dwPageSize == b.dwPageSize, a.dwAllocationGranularity == b.dwAllocationGranularity,
           a.lpMinimumApplicationAddress == b.lpMinimumApplicationAddress, a.lpMaximumApplicationAddress == b.lpMaximumApplicationAddress,
           a.dwActiveProcessorMask == b.dwActiveProcessorMask, a.dwNumberOfProcessors == b.dwNumberOfProcessors,
           a.wProcessorLevel == b.wProcessorLevel, a.wProcessorRevision == b.wProcessorRevision);

    char d[MAX_PATH];
    WCHAR wd[MAX_PATH];
    SetLastError(0xdeadbeef);
    UINT n = GetSystemWow64DirectoryA(d, sizeof d);
    if (n) lower(d); else strcpy(d, "-");
    printf("GetSystemWow64DirectoryA %u err %lu %s\n", n, n ? 0 : GetLastError(), d);
    if (n) {
        UINT caps[] = { 0, 1, n - 1, n, n + 1 };
        for (int i = 0; i < 5; i++) {
            memset(d, 'x', sizeof d);
            UINT got = GetSystemWow64DirectoryA(caps[i] ? d : NULL, caps[i]);
            printf("  size %u: %u first %s\n", caps[i], got, d[0] == 'x' ? "untouched" : "written");
        }
    }
    SetLastError(0xdeadbeef);
    n = GetSystemWow64DirectoryW(wd, MAX_PATH);
    printf("GetSystemWow64DirectoryW %u err %lu same as A %d\n", n, n ? 0 : GetLastError(), n && (WideCharToMultiByte(CP_ACP, 0, wd, -1, d, sizeof d, NULL, NULL), lower(d), !strcmp(d, "c:\\windows\\syswow64")));
    dir2a_t dir2 = (dir2a_t)GetProcAddress(kb, "GetSystemWow64Directory2A");
    if (dir2) {
        WORD machines[] = { 0x14c, 0x8664, 0x1c4, 0xaa64, 0 };
        for (int i = 0; i < 5; i++) {
            SetLastError(0xdeadbeef);
            n = dir2(d, sizeof d, machines[i]);
            if (n) lower(d); else strcpy(d, "-");
            printf("GetSystemWow64Directory2A(%#x) %u err %lu %s\n", machines[i], n, n ? 0 : GetLastError(), d);
        }
    }
    GetSystemDirectoryA(d, sizeof d);
    lower(d);
    printf("GetSystemDirectory %s\n", d);

    env("ProgramFiles");
    env("ProgramFiles(x86)");
    env("ProgramW6432");
    env("CommonProgramFiles");
    env("CommonProgramFiles(x86)");
    env("CommonProgramW6432");
    env("PROCESSOR_ARCHITECTURE");
    env("PROCESSOR_ARCHITEW6432");

    csidl(CSIDL_SYSTEM);
    csidl(CSIDL_SYSTEMX86);
    csidl(CSIDL_PROGRAM_FILES);
    csidl(CSIDL_PROGRAM_FILESX86);
    csidl(CSIDL_PROGRAM_FILES_COMMON);
    csidl(CSIDL_PROGRAM_FILES_COMMONX86);
    static const GUID x64 = { 0x6d809377, 0x6af0, 0x444b, { 0x89, 0x57, 0xa3, 0x77, 0x3f, 0x02, 0x20, 0x0e } };
    static const GUID x86 = { 0x7c5a40ef, 0xa0fb, 0x4bfc, { 0x87, 0x4a, 0xc0, 0xf2, 0xe0, 0xb9, 0xfa, 0x8e } };
    static const GUID sysx86 = { 0xd65231b0, 0xb2f1, 0x4857, { 0xa4, 0xce, 0xa8, 0xe7, 0xc6, 0xea, 0x7d, 0x27 } };
    static const GUID cx64 = { 0x6365d5a7, 0x0f0d, 0x45e5, { 0x87, 0xf6, 0x0d, 0xa5, 0x6b, 0x6a, 0x4f, 0x7d } };
    static const GUID cx86 = { 0xde974d24, 0xd9c6, 0x4d3e, { 0xbf, 0x91, 0xf4, 0x45, 0x51, 0x20, 0xb9, 0x17 } };
    known("ProgramFilesX64", &x64);
    known("ProgramFilesX86", &x86);
    known("ProgramFilesCommonX64", &cx64);
    known("ProgramFilesCommonX86", &cx86);
    known("SystemX86", &sysx86);

    regval("default", 0, "ProgramFilesDir");
    regval("64KEY", KEY_WOW64_64KEY, "ProgramFilesDir");
    regval("32KEY", KEY_WOW64_32KEY, "ProgramFilesDir");
    regval("default", 0, "CommonFilesDir");
    regval("64KEY", KEY_WOW64_64KEY, "CommonFilesDir");
    regval("default", 0, "ProgramFilesDir (x86)");
    regval("default", 0, "ProgramW6432Dir");
    {
        HKEY k;
        LONG s = RegOpenKeyExA(HKEY_LOCAL_MACHINE, "SOFTWARE\\Wow6432Node", 0, KEY_READ, &k);
        printf("open SOFTWARE\\Wow6432Node %ld\n", s);
        if (!s) RegCloseKey(k);
        s = RegOpenKeyExA(HKEY_LOCAL_MACHINE, "SOFTWARE\\Wow6432Node\\Microsoft\\Windows\\CurrentVersion", 0, KEY_READ, &k);
        printf("open SOFTWARE\\Wow6432Node\\Microsoft\\Windows\\CurrentVersion %ld\n", s);
        if (!s) RegCloseKey(k);
    }

    attrs("C:\\Windows\\SysWOW64");
    attrs("C:\\Windows\\SysWOW64\\kernel32.dll");
    attrs("C:\\Windows\\System32\\kernel32.dll");
    attrs("C:\\Windows\\Sysnative\\kernel32.dll");
    {
        void *old = NULL;
        BOOL off = Wow64DisableWow64FsRedirection(&old);
        attrs("C:\\Windows\\System32\\kernel32.dll");
        if (off) Wow64RevertWow64FsRedirection(old);
    }

    nqip_t q = (nqip_t)GetProcAddress(GetModuleHandleA("ntdll.dll"), "NtQueryInformationProcess");
    ULONG_PTR v = 0xcccc;
    ULONG got = 0;
    LONG st = q(GetCurrentProcess(), 26, &v, sizeof v, &got);
    printf("ProcessWow64Information status %#lx nonzero %d length %lu\n", (unsigned long)st, v != 0, got);
    st = q(GetCurrentProcess(), 26, &v, sizeof v - 1, &got);
    printf("ProcessWow64Information, short: status %#lx\n", (unsigned long)st);
    return 0;
}
