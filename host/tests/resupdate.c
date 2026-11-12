/* libs: -lkernel32 */
/* Resource updates as installer compilers make them on their stubs (Inno Setup's ISCC writes
 * icons and the version into SetupLdr): a copy of this program gets an RCDATA in two languages,
 * a named type and a "#7" name, has one removed again, and is read back with
 * LoadLibraryEx(LOAD_LIBRARY_AS_DATAFILE); the updated copy still runs and finds its own new
 * resource; a discarded update leaves the file as it was, and deleting the existing resources
 * leaves only the new one. The error for a missing file.
 * (sake: resupdate.rs; compared with Windows by tests/winref)
 */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static void show(HMODULE m, const char *what, LPCSTR type, LPCSTR name, WORD lang) {
    HRSRC r = FindResourceExA(m, type, name, lang);
    if (!r) {
        printf("  %s: not found (%lu)\n", what, GetLastError());
        return;
    }
    DWORD n = SizeofResource(m, r);
    const char *p = LockResource(LoadResource(m, r));
    printf("  %s: %lu bytes \"%.*s\"\n", what, n, (int)n, p ? p : "");
}

static DWORD file_size(const char *f) {
    WIN32_FILE_ATTRIBUTE_DATA a;
    return GetFileAttributesExA(f, GetFileExInfoStandard, &a) ? a.nFileSizeLow : 0;
}

int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "child")) {
        show(NULL, "the copy's own resource", (LPCSTR)RT_RCDATA, "GREETING", MAKELANGID(LANG_ENGLISH, SUBLANG_ENGLISH_US));
        return 7;
    }
    char self[MAX_PATH], tmp[MAX_PATH], copy[MAX_PATH];
    GetModuleFileNameA(NULL, self, sizeof self);
    GetTempPathA(sizeof tmp, tmp);
    sprintf(copy, "%sresupdate%lu.exe", tmp, GetCurrentProcessId());
    CopyFileA(self, copy, FALSE);
    WORD en = MAKELANGID(LANG_ENGLISH, SUBLANG_ENGLISH_US), ja = MAKELANGID(LANG_JAPANESE, SUBLANG_DEFAULT);

    HANDLE u = BeginUpdateResourceA(copy, FALSE);
    printf("begin: %s\n", u ? "ok" : "failed");
    printf("rcdata en: %d\n", UpdateResourceA(u, (LPCSTR)RT_RCDATA, "Greeting", en, "hello", 5));
    printf("rcdata ja: %d\n", UpdateResourceA(u, (LPCSTR)RT_RCDATA, "GREETING", ja, "konnichiwa", 10));
    printf("named type: %d\n", UpdateResourceA(u, "MyType", "#7", 0, "seven", 5));
    printf("replaced: %d\n", UpdateResourceA(u, "MYTYPE", MAKEINTRESOURCEA(7), 0, "SEVEN!", 6));
    printf("to remove: %d\n", UpdateResourceA(u, (LPCSTR)RT_RCDATA, "GONE", 0, "x", 1));
    printf("removed: %d\n", UpdateResourceA(u, (LPCSTR)RT_RCDATA, "GONE", 0, NULL, 0));
    printf("remove what is not there: %d\n", UpdateResourceA(u, (LPCSTR)RT_RCDATA, "NOTHING", 0, NULL, 0));
    printf("end: %d\n", EndUpdateResourceA(u, FALSE));

    HMODULE m = LoadLibraryExA(copy, NULL, LOAD_LIBRARY_AS_DATAFILE);
    printf("load as data: %s\n", m ? "ok" : "failed");
    show(m, "rcdata en", (LPCSTR)RT_RCDATA, "GREETING", en);
    show(m, "rcdata ja", (LPCSTR)RT_RCDATA, "greeting", ja);
    show(m, "named type", "MYTYPE", MAKEINTRESOURCEA(7), 0);
    show(m, "removed", (LPCSTR)RT_RCDATA, "GONE", 0);
    FreeLibrary(m);

    STARTUPINFOA si = {sizeof si};
    PROCESS_INFORMATION pi;
    char cmd[2 * MAX_PATH];
    sprintf(cmd, "\"%s\" child", copy);
    fflush(stdout);
    if (CreateProcessA(copy, cmd, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi)) {
        WaitForSingleObject(pi.hProcess, 30000);
        DWORD code = 0;
        GetExitCodeProcess(pi.hProcess, &code);
        printf("the copy ran: exit %lu\n", code);
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
    } else {
        printf("the copy did not start (%lu)\n", GetLastError());
    }

    DWORD before = file_size(copy);
    u = BeginUpdateResourceA(copy, FALSE);
    UpdateResourceA(u, (LPCSTR)RT_RCDATA, "BIG", 0, copy, 100);
    printf("discarded: %d, size the same: %d\n", EndUpdateResourceA(u, TRUE), file_size(copy) == before);

    u = BeginUpdateResourceA(copy, TRUE);
    UpdateResourceA(u, (LPCSTR)RT_RCDATA, "ONLY", 0, "alone", 5);
    printf("deleting the rest: %d\n", EndUpdateResourceA(u, FALSE));
    m = LoadLibraryExA(copy, NULL, LOAD_LIBRARY_AS_DATAFILE);
    show(m, "only", (LPCSTR)RT_RCDATA, "ONLY", 0);
    show(m, "rcdata en after", (LPCSTR)RT_RCDATA, "GREETING", en);
    FreeLibrary(m);

    char missing[MAX_PATH];
    sprintf(missing, "%snot-there-%lu.exe", tmp, GetCurrentProcessId());
    SetLastError(0);
    u = BeginUpdateResourceA(missing, FALSE);
    printf("missing file: %s, error %lu\n", u ? "a handle" : "NULL", GetLastError());
    /* A corrupt FileAlignment must not make a small update allocate gigabytes. */
    HANDLE f = CreateFileA(copy, GENERIC_READ | GENERIC_WRITE, 0, NULL, OPEN_EXISTING, 0, NULL);
    DWORD nt = 0, bytes = 0, alignment = 0x86000200;
    if (f == INVALID_HANDLE_VALUE) return 8;
    SetFilePointer(f, 0x3c, NULL, FILE_BEGIN);
    if (!ReadFile(f, &nt, 4, &bytes, NULL) || bytes != 4) return 9;
    SetFilePointer(f, nt + 24 + 36, NULL, FILE_BEGIN);
    if (!WriteFile(f, &alignment, 4, &bytes, NULL) || bytes != 4) return 10;
    CloseHandle(f);
    before = file_size(copy);
    u = BeginUpdateResourceA(copy, FALSE);
    if (u) {
        if (!UpdateResourceA(u, (LPCSTR)RT_RCDATA, "ALIGNMENT", 0, "x", 1)) {
            EndUpdateResourceA(u, TRUE);
        } else {
            EndUpdateResourceA(u, FALSE);
        }
    }
    /* Windows can accept this update without growing the existing section.
       For a malformed PE, test the allocation bound, not a required rejection. */
    WIN32_FILE_ATTRIBUTE_DATA after;
    BOOL bounded = GetFileAttributesExA(copy, GetFileExInfoStandard, &after)
        && after.nFileSizeHigh == 0 && after.nFileSizeLow >= before
        && after.nFileSizeLow <= before + 0x10000;
    printf("malformed alignment: size bounded %d\n", bounded);
    DeleteFileA(copy);
    return bounded ? 0 : 11;
}
