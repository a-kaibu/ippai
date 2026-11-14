/* An Inno Setup installer (libs: -ladvapi32 -lshell32 -lole32 -luuid), innoinstall-setup.exe,
 * compiled by tests/host-tests.sh from innoinstall.iss with Inno Setup 6.7.4's ISCC running on
 * sake (Inno Setup itself installed there by its own installer): run silently, it writes the
 * files, a registry value, a Start menu shortcut (a ShellLink saved as a .lnk) and its
 * uninstaller with the Uninstall key. The uninstaller copies
 * itself to %TEMP%, the copy removes everything and tells the first phase to go
 * (SendNotifyMessage), then removes the folder. (stdout fully buffered and written at the end:
 * the uninstaller's own processes write to the same log while this one waits) */
#define COBJMACROS
#include <windows.h>
#include <shlobj.h>
#include <shobjidl.h>
#include <stdio.h>
#include <string.h>

static char dir[MAX_PATH], lnk[MAX_PATH];

static DWORD run(const char *cmd) {
    STARTUPINFOA si = {sizeof si};
    PROCESS_INFORMATION pi;
    char line[2 * MAX_PATH];
    strcpy(line, cmd);
    if (!CreateProcessA(NULL, line, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi)) {
        printf("  CreateProcess failed: %lu\n", GetLastError());
        return (DWORD)-1;
    }
    WaitForSingleObject(pi.hProcess, 300000);
    DWORD code = 0;
    GetExitCodeProcess(pi.hProcess, &code);
    CloseHandle(pi.hProcess);
    CloseHandle(pi.hThread);
    return code;
}

static int exists(const char *rel) {
    char p[MAX_PATH];
    snprintf(p, sizeof p, "%s%s", dir, rel);
    return GetFileAttributesA(p) != INVALID_FILE_ATTRIBUTES;
}

static void shown(const char *p) {
    printf("%s", !_strnicmp(p, dir, strlen(dir) - 1) ? "<app>" : "");
    printf("%s", !_strnicmp(p, dir, strlen(dir) - 1) ? p + strlen(dir) - 1 : p);
}

static void state(const char *what) {
    printf("%s\n", what);
    const char *files[] = {"a.txt", "sub\\b.dat", "unins000.exe", "unins000.dat", ""};
    for (int i = 0; i < 5; i++)
        printf("  %s: %s\n", *files[i] ? files[i] : "(folder)", exists(files[i]) ? "there" : "missing");
    char v[MAX_PATH] = "";
    DWORD n = sizeof v;
    LONG r = RegGetValueA(HKEY_CURRENT_USER, "Software\\masu\\innotest", "path", RRF_RT_REG_SZ, NULL, v, &n);
    strcat(v, "\\");
    printf("  HKCU path: %ld %s\n", r, r ? "" : _stricmp(v, dir) ? v : "<app>");
    n = sizeof v;
    r = RegGetValueA(HKEY_LOCAL_MACHINE, "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\InnoTest_is1", "UninstallString", RRF_RT_REG_SZ, NULL, v, &n);
    printf("  UninstallString: %ld", r);
    if (!r) {
        printf(" ");
        shown(v);
    }
    printf("\n");
    printf("  shortcut: %s\n", GetFileAttributesA(lnk) != INVALID_FILE_ATTRIBUTES ? "there" : "missing");
    IPersistFile *pf;
    if (GetFileAttributesA(lnk) == INVALID_FILE_ATTRIBUTES || CoCreateInstance(&CLSID_ShellLink, NULL, CLSCTX_INPROC_SERVER, &IID_IPersistFile, (void **)&pf))
        return;
    WCHAR w[MAX_PATH];
    MultiByteToWideChar(CP_ACP, 0, lnk, -1, w, MAX_PATH);
    HRESULT hr = IPersistFile_Load(pf, w, STGM_READ);
    IShellLinkA *sl;
    if (!hr && !IPersistFile_QueryInterface(pf, &IID_IShellLinkA, (void **)&sl)) {
        char p[MAX_PATH];
        IShellLinkA_GetPath(sl, p, MAX_PATH, NULL, SLGP_RAWPATH);
        printf("    target ");
        shown(p);
        IShellLinkA_GetArguments(sl, p, MAX_PATH);
        printf(", arguments %s", p);
        IShellLinkA_GetWorkingDirectory(sl, p, MAX_PATH);
        printf(", in ");
        shown(p);
        IShellLinkA_GetDescription(sl, p, MAX_PATH);
        printf(", \"%s\"\n", p);
        IShellLinkA_Release(sl);
    } else {
        printf("    load: %lx\n", hr);
    }
    IPersistFile_Release(pf);
}

int main(void) {
    static char out[65536];
    setvbuf(stdout, out, _IOFBF, sizeof out);
    CoInitialize(NULL);
    char setup[MAX_PATH], cmd[2 * MAX_PATH];
    SHGetFolderPathA(NULL, CSIDL_PROGRAM_FILESX86, NULL, 0, dir); /* (the 32-bit installer's Program Files, on 64-bit Windows from either kind of process) */
    strcat(dir, "\\InnoTest\\");
    SHGetFolderPathA(NULL, CSIDL_COMMON_PROGRAMS, NULL, 0, lnk);
    strcat(lnk, "\\InnoTest\\InnoTest.lnk");
    GetModuleFileNameA(NULL, setup, sizeof setup);
    strcpy(strrchr(setup, '\\') + 1, "innoinstall-setup.exe");
    snprintf(cmd, sizeof cmd, "\"%s\" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /LOG=C:\\windows\\temp\\inno-install.log", setup);
    DWORD installed = run(cmd);
    printf("install: %lu\n", installed);
    if (installed) {
        /* Keep the installer's reason with the harness log, not just its exit code. */
        FILE *log = fopen("C:\\windows\\temp\\inno-install.log", "rb");
        if (log) {
            char buf[4096];
            size_t n;
            while ((n = fread(buf, 1, sizeof buf, log))) fwrite(buf, 1, n, stderr);
            fclose(log);
        }
    }
    state("installed");
    snprintf(cmd, sizeof cmd, "\"%sunins000.exe\" /VERYSILENT /SUPPRESSMSGBOXES", dir);
    printf("uninstall: %lu\n", run(cmd));
    /* (the copy in %TEMP% does the work while and after the first process goes) */
    for (int i = 0; i < 1200 && exists(""); i++)
        Sleep(100);
    state("uninstalled");
    CoUninitialize();
    return 0;
}
