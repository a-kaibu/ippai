/* msi.dll installing packages (libs: -lmsi -ladvapi32 -lshell32): msiinstall.msi (files in its embedded
 * cabinet) and msiinstall-u.msi (the same, its files uncompressed beside it in
 * MsiTest\ and MsiTest\sub\, as InstallShield's "uncompressed" releases are), both built
 * by tests/host-tests.sh from msiinstall.wxs with wixl. Each is installed, has a feature
 * added and removed, is queried (MsiQueryProductState, MsiGetProductInfo,
 * MsiEnumRelatedProducts) and uninstalled (REMOVE=ALL): the files, registry values,
 * folders and registration come and go. AppSearch finds a REG_SZ starting with '#', a
 * DWORD, a folder named in the registry and a file one folder down (DrLocator), and its
 * results are written back as a registry value. Disk costs (MsiEnumComponentCosts,
 * MsiGetFeatureCost) are asked of the package opened and costed. */
#include <windows.h>
#include <msi.h>
#include <msiquery.h>
#include <shlobj.h>
#include <stdio.h>
#include <string.h>

static const char *CODE = "{6A0F2E9B-5C1D-4E7A-9B3C-1D2E3F405162}";
static const char *UPGRADE = "{0B1C2D3E-4F50-6172-8394-A5B6C7D8E9F0}";
static char dir[MAX_PATH];

static void file(const char *rel) {
    char p[MAX_PATH];
    snprintf(p, sizeof p, "%s%s", dir, rel);
    HANDLE h = CreateFileA(p, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    if (h == INVALID_HANDLE_VALUE) {
        printf("  %s: missing\n", rel);
        return;
    }
    char b[16] = {0};
    DWORD n = 0, size = GetFileSize(h, NULL);
    ReadFile(h, b, 9, &n, NULL);
    CloseHandle(h);
    for (DWORD i = 0; i < n; i++)
        if (b[i] < ' ') b[i] = '.';
    printf("  %s: %lu bytes \"%s\"\n", rel, size, b);
}

static void folder(const char *rel) {
    char p[MAX_PATH];
    snprintf(p, sizeof p, "%s%s", dir, rel);
    DWORD a = GetFileAttributesA(p);
    printf("  %s\\: %s\n", rel, a == INVALID_FILE_ATTRIBUTES ? "missing" : (a & FILE_ATTRIBUTE_DIRECTORY) ? "folder" : "file");
}

static void registry(void) {
    HKEY k;
    LONG r = RegOpenKeyExA(HKEY_CURRENT_USER, "Software\\masu\\msitest", 0, KEY_READ, &k);
    if (r) {
        printf("  HKCU\\Software\\masu\\msitest: missing (%ld)\n", r);
        return;
    }
    char s[MAX_PATH] = "";
    DWORD ty = 0, n = sizeof s, v = 0;
    r = RegQueryValueExA(k, "path", NULL, &ty, (BYTE *)s, &n);
    printf("  path: %ld type %lu \"%s\"\n", r, ty, r ? "" : (strcmp(s, dir) ? s : "<INSTALLDIR>"));
    n = sizeof v;
    r = RegQueryValueExA(k, "count", NULL, &ty, (BYTE *)&v, &n);
    printf("  count: %ld type %lu %lu\n", r, ty, v);
    n = sizeof s;
    r = RegQueryValueExA(k, "found", NULL, &ty, (BYTE *)s, &n);
    if (!r) {
        /* (the paths under Program Files as <PF>) */
        char pf[MAX_PATH], out[2 * MAX_PATH] = "", *p = s;
        SHGetFolderPathA(NULL, CSIDL_PROGRAM_FILESX86, NULL, 0, pf); /* (the 32-bit package's ProgramFilesFolder, on 64-bit Windows from either kind of process) */
        size_t l = strlen(pf);
        while (*p) {
            if (!_strnicmp(p, pf, l)) {
                strcat(out, "<PF>");
                p += l;
            } else
                strncat(out, p++, 1);
        }
        printf("  found: \"%s\"\n", out);
    } else
        printf("  found: %ld\n", r);
    RegCloseKey(k);
}

static void product(void) {
    printf("  state %d\n", MsiQueryProductStateA(CODE));
    char b[MAX_PATH];
    const char *props[] = {INSTALLPROPERTY_VERSIONSTRING, INSTALLPROPERTY_INSTALLEDPRODUCTNAME, INSTALLPROPERTY_PUBLISHER};
    for (int i = 0; i < 3; i++) {
        DWORD n = sizeof b;
        UINT r = MsiGetProductInfoA(CODE, props[i], b, &n);
        printf("  %s: %u \"%s\" %lu\n", props[i], r, r ? "" : b, r ? 0 : n);
    }
    char related[39] = "";
    UINT r = MsiEnumRelatedProductsA(UPGRADE, 0, 0, related);
    printf("  related: %u %s\n", r, r ? "" : related);
    for (int i = 0; i < 3; i++) {
        const char *f[] = {"Complete", "Extras", "Nope"};
        printf("  feature %s: %d\n", f[i], MsiQueryFeatureStateA(CODE, f[i]));
    }
    /* (the components' key files, as <INSTALLDIR>...) */
    const char *comps[] = {"{11111111-2222-3333-4444-555555555555}", "{33333333-4444-5555-6666-777777777777}"};
    for (int i = 0; i < 2; i++) {
        DWORD n = sizeof b;
        b[0] = 0;
        INSTALLSTATE st = MsiGetComponentPathA(CODE, comps[i], b, &n);
        char m[MAX_PATH] = "";
        DWORD mn = sizeof m;
        INSTALLSTATE lst = MsiLocateComponentA(comps[i], m, &mn);
        size_t l = strlen(dir);
        printf("  component %d: %d \"%s%s\" locate %d %s\n", i, st, strncmp(b, dir, l) ? "" : "<INSTALLDIR>", strncmp(b, dir, l) ? b : b + l, lst, strcmp(m, b) ? "differs" : "same");
    }
}

static void state(const char *what) {
    printf("%s\n", what);
    file("a.txt");
    file("sub\\b.dat");
    file("extra.txt");
    folder("sub");
    folder("");
    registry();
    product();
}

/* Disk costs (512-byte units, files in whole 4 KiB clusters) once InstallValidate ran. */
static void costs(void) {
    char path[MAX_PATH], drive[8];
    GetModuleFileNameA(NULL, path, sizeof path);
    strcpy(strrchr(path, '\\') + 1, "msiinstall.msi");
    MSIHANDLE h;
    UINT r = MsiOpenPackageA(path, &h);
    printf("costs: open %u\n", r);
    if (r)
        return;
    int cost = -1, temp = -1;
    DWORD n = sizeof drive;
    printf("  before costing: %u\n", MsiEnumComponentCostsA(h, "Data", 0, INSTALLSTATE_LOCAL, drive, &n, &cost, &temp));
    MsiDoActionA(h, "CostInitialize");
    MsiDoActionA(h, "FileCost");
    MsiDoActionA(h, "CostFinalize");
    printf("  not validated: %u\n", MsiEnumComponentCostsA(h, "Data", 0, INSTALLSTATE_LOCAL, drive, &n, &cost, &temp));
    MsiDoActionA(h, "InstallValidate");
    const char *comps[] = {"Main", "Data", "Extra", "Nope"};
    for (int i = 0; i < 4; i++) {
        n = sizeof drive;
        cost = temp = -1;
        r = MsiEnumComponentCostsA(h, comps[i], 0, INSTALLSTATE_LOCAL, drive, &n, &cost, &temp);
        printf("  component %s: %u %s %d %d\n", comps[i], r, r ? "" : drive, cost, temp);
    }
    n = sizeof drive;
    printf("  index 1: %u\n", MsiEnumComponentCostsA(h, "Data", 1, INSTALLSTATE_LOCAL, drive, &n, &cost, &temp));
    n = 1;
    r = MsiEnumComponentCostsA(h, "Data", 0, INSTALLSTATE_LOCAL, drive, &n, &cost, &temp);
    printf("  small buffer: %u %lu\n", r, n);
    const char *feats[] = {"Complete", "Extras", "Nope"};
    for (int i = 0; i < 3; i++)
        for (int tree = 0; tree < 3; tree++) {
            cost = -1;
            r = MsiGetFeatureCostA(h, feats[i], tree, INSTALLSTATE_LOCAL, &cost);
            printf("  feature %s tree %d: %u %d\n", feats[i], tree, r, cost);
        }
    printf("  bad tree: %u\n", MsiGetFeatureCostA(h, "Complete", 3, INSTALLSTATE_LOCAL, &cost));
    MsiCloseHandle(h);
}

static void run(const char *msi) {
    char path[MAX_PATH];
    GetModuleFileNameA(NULL, path, sizeof path);
    strcpy(strrchr(path, '\\') + 1, msi);
    printf("== %s\n", msi);
    state("before");
    const char *steps[][2] = {
        {"install", ""},
        {"add Extras", "ADDLOCAL=Extras"},
        {"remove Extras", "REMOVE=Extras"},
        {"uninstall", "REMOVE=ALL"},
    };
    for (int i = 0; i < 4; i++) {
        /* (the uncompressed one is removed through its cached package) */
        UINT r = i == 3 && strstr(msi, "-u") ? MsiConfigureProductA(CODE, INSTALLLEVEL_DEFAULT, INSTALLSTATE_ABSENT) : MsiInstallProductA(path, steps[i][1]);
        char what[64];
        snprintf(what, sizeof what, "%s: %u", steps[i][0], r);
        state(what);
    }
}

/* What AppSearch is to find. */
static void search_setup(void) {
    char pf[MAX_PATH], p[MAX_PATH];
    SHGetFolderPathA(NULL, CSIDL_PROGRAM_FILESX86, NULL, 0, pf); /* (the 32-bit package's ProgramFilesFolder, on 64-bit Windows from either kind of process) */
    snprintf(p, sizeof p, "%s\\MsiSearch", pf);
    CreateDirectoryA(p, NULL);
    strcat(p, "\\deep");
    CreateDirectoryA(p, NULL);
    strcat(p, "\\probe.txt");
    HANDLE h = CreateFileA(p, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    CloseHandle(h);
    HKEY k;
    RegCreateKeyExA(HKEY_CURRENT_USER, "Software\\masu\\search", 0, NULL, 0, KEY_ALL_ACCESS, NULL, &k, NULL);
    RegSetValueExA(k, "sz", 0, REG_SZ, (const BYTE *)"#hash", 6);
    DWORD v = 0xfffffff6;
    RegSetValueExA(k, "dw", 0, REG_DWORD, (const BYTE *)&v, 4);
    snprintf(p, sizeof p, "%s\\MsiSearch", pf);
    RegSetValueExA(k, "dir", 0, REG_SZ, (const BYTE *)p, strlen(p) + 1);
    RegCloseKey(k);
}

int main(void) {
    SHGetFolderPathA(NULL, CSIDL_PROGRAM_FILESX86, NULL, 0, dir); /* (the 32-bit package's ProgramFilesFolder, on 64-bit Windows from either kind of process) */
    strcat(dir, "\\MsiTest\\");
    search_setup();
    printf("ui %d\n", MsiSetInternalUI(INSTALLUILEVEL_NONE, NULL));
    printf("ui %d\n", MsiSetInternalUI(INSTALLUILEVEL_NONE | INSTALLUILEVEL_ENDDIALOG, NULL));
    printf("ui %d\n", MsiSetInternalUI(6, NULL));
    printf("ui %d\n", MsiSetInternalUI(INSTALLUILEVEL_NONE, NULL));
    printf("missing package: %u\n", MsiInstallProductA("C:\\nowhere\\none.msi", ""));
    printf("configure unknown: %u\n", MsiConfigureProductA(CODE, INSTALLLEVEL_DEFAULT, INSTALLSTATE_ABSENT));
    char v[32] = "x", lang[16] = "x";
    DWORD vn = sizeof v, ln = sizeof lang;
    printf("file version missing: %u\n", MsiGetFileVersionA("C:\\nowhere\\none.dll", v, &vn, lang, &ln));
    costs();
    run("msiinstall.msi");
    run("msiinstall-u.msi");
    /* msiexec as a process of its own, as installers and uninstall strings run it */
    char path[MAX_PATH], cmd[2 * MAX_PATH];
    GetModuleFileNameA(NULL, path, sizeof path);
    strcpy(strrchr(path, '\\') + 1, "msiinstall.msi");
    const char *cmds[] = {"msiexec /i \"%s\" /qn TESTPROP=fromcmd", "MsiExec.exe /X{6A0F2E9B-5C1D-4E7A-9B3C-1D2E3F405162} /qn", "msiexec /i"};
    for (int i = 0; i < 3; i++) {
        snprintf(cmd, sizeof cmd, cmds[i], path);
        STARTUPINFOA si = {sizeof si};
        PROCESS_INFORMATION pi;
        DWORD code = 0xdead;
        if (CreateProcessA(NULL, cmd, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi)) {
            WaitForSingleObject(pi.hProcess, INFINITE);
            GetExitCodeProcess(pi.hProcess, &code);
            CloseHandle(pi.hProcess);
            CloseHandle(pi.hThread);
        }
        printf("msiexec %d: %lu\n", i, code);
        printf("  state %d\n", MsiQueryProductStateA(CODE));
        file("a.txt");
    }
    return 0;
}
