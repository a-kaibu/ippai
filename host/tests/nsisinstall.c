/* An NSIS installer (libs: -ladvapi32 -lshell32), nsisinstall-setup.exe, built by
 * tests/host-tests.sh from nsisinstall.nsi with makensis: run silently (/S) as a process of
 * its own, it unpacks its LZMA data, writes the files, a registry value, its uninstaller and
 * the Uninstall key, and leaves nothing in %TEMP% (its data file is open with
 * FILE_FLAG_DELETE_ON_CLOSE, its plug-in folder removed). The uninstaller runs in place
 * (_?=) and then as usual, copying itself to %TEMP% and starting the copy, which removes
 * everything (the copy itself stays for the next reboot, as on Windows). */
#include <windows.h>
#include <shlobj.h>
#include <stdio.h>
#include <string.h>

static char dir[MAX_PATH], tmp[MAX_PATH];

static DWORD run(const char *cmd) {
    STARTUPINFOA si = {sizeof si};
    PROCESS_INFORMATION pi;
    char line[2 * MAX_PATH];
    strcpy(line, cmd);
    if (!CreateProcessA(NULL, line, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi)) {
        printf("  CreateProcess failed: %lu\n", GetLastError());
        return (DWORD)-1;
    }
    WaitForSingleObject(pi.hProcess, 120000);
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

static void state(const char *what) {
    printf("%s\n", what);
    const char *files[] = {"a.txt", "sub\\b.dat", "sub", "uninst.exe", ""};
    for (int i = 0; i < 5; i++)
        printf("  %s: %s\n", *files[i] ? files[i] : "(folder)", exists(files[i]) ? "there" : "missing");
    char v[MAX_PATH] = "";
    DWORD n = sizeof v;
    LONG r = RegGetValueA(HKEY_CURRENT_USER, "Software\\masu\\nsistest", "path", RRF_RT_REG_SZ, NULL, v, &n);
    strcat(v, "\\");
    printf("  HKCU path: %ld %s\n", r, r ? "" : strcmp(v, dir) ? v : "<INSTDIR>");
    n = sizeof v;
    r = RegGetValueA(HKEY_LOCAL_MACHINE, "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\NsisTest", "UninstallString", RRF_RT_REG_SZ, NULL, v, &n);
    printf("  UninstallString: %ld\n", r);
    /* what is left in %TEMP% (the uninstaller's own copy, ~nsu*.tmp, apart) */
    char mask[MAX_PATH];
    snprintf(mask, sizeof mask, "%s*", tmp);
    WIN32_FIND_DATAA fd;
    HANDLE h = FindFirstFileA(mask, &fd);
    int left = 0, copies = 0;
    if (h != INVALID_HANDLE_VALUE) {
        do {
            if (!strcmp(fd.cFileName, ".") || !strcmp(fd.cFileName, ".."))
                continue;
            if (!strncmp(fd.cFileName, "~nsu", 4))
                copies++;
            else {
                left++;
                printf("  left in %%TEMP%%: %s\n", fd.cFileName);
            }
        } while (FindNextFileA(h, &fd));
        FindClose(h);
    }
    printf("  %%TEMP%%: %d left, %d uninstaller copies\n", left, copies);
}

int main(void) {
    char setup[MAX_PATH], cmd[2 * MAX_PATH];
    SHGetFolderPathA(NULL, CSIDL_PROGRAM_FILESX86, NULL, 0, dir); /* (the 32-bit installer's Program Files, on 64-bit Windows from either kind of process) */
    strcat(dir, "\\NsisTest\\");
    GetTempPathA(sizeof tmp, tmp);
    /* (an earlier run's uninstaller copies, which Windows removes at the next reboot) */
    for (int i = 0; i < 16; i++) {
        char p[MAX_PATH];
        snprintf(p, sizeof p, "%s~nsu%d.tmp\\Un.exe", tmp, i);
        DeleteFileA(p);
        *strrchr(p, '\\') = 0;
        RemoveDirectoryA(p);
    }
    GetModuleFileNameA(NULL, setup, sizeof setup);
    strcpy(strrchr(setup, '\\') + 1, "nsisinstall-setup.exe");
    snprintf(cmd, sizeof cmd, "\"%s\" /S", setup);
    printf("install: %lu\n", run(cmd));
    state("installed");
    snprintf(cmd, sizeof cmd, "\"%suninst.exe\" /S _?=%.*s", dir, (int)strlen(dir) - 1, dir);
    printf("uninstall in place: %lu\n", run(cmd));
    state("uninstalled in place");
    snprintf(cmd, sizeof cmd, "\"%s\" /S", setup);
    printf("install again: %lu\n", run(cmd));
    snprintf(cmd, sizeof cmd, "\"%suninst.exe\" /S", dir);
    printf("uninstall: %lu\n", run(cmd));
    /* (the copy in %TEMP% does the work after the first process has gone) */
    for (int i = 0; i < 600 && exists(""); i++)
        Sleep(100);
    state("uninstalled");
    return 0;
}
