/* libs: -lshell32 -ladvapi32 */
/* Explorer's folder lists in the registry, which older programs read instead of calling
 * SHGetFolderPath (EntisGLS' WitchWizard, #162: HKCU ...\Explorer\Shell Folders "AppData" and
 * "Personal"; with no key it saved under "\SYRUP\..." at the root of the drive). For each value
 * name: the REG_SZ in "Shell Folders" (HKCU for the user's folders, HKLM for the common ones),
 * whether it is the folder SHGetFolderPath gives for its CSIDL, and the REG_EXPAND_SZ in "User
 * Shell Folders" expanded the same way. Paths are shown from the environment as in shfolders.c.
 * (sake: misc.rs SHELL_FOLDERS; compared with Windows by tests/winref)
 */
#include <windows.h>
#include <shlobj.h>
#include <stdio.h>
#include <string.h>

static const char *vars[] = {"APPDATA", "LOCALAPPDATA", "PUBLIC", "ProgramData", "USERPROFILE", "windir"};

static void shown(const char *p, char *out) {
    for (int i = 0; i < 6; i++) {
        char v[MAX_PATH];
        DWORD n = GetEnvironmentVariableA(vars[i], v, sizeof v);
        if (n && !_strnicmp(p, v, n) && (p[n] == 0 || p[n] == '\\')) {
            sprintf(out, "%%%s%%%s", vars[i], p + n);
            return;
        }
    }
    strcpy(out, p);
}

static const struct {
    const char *name;
    int csidl;
    int user;
} F[] = {
    {"AppData", CSIDL_APPDATA, 1},
    {"Local AppData", CSIDL_LOCAL_APPDATA, 1},
    {"Personal", CSIDL_PERSONAL, 1},
    {"Desktop", CSIDL_DESKTOPDIRECTORY, 1},
    {"Programs", CSIDL_PROGRAMS, 1},
    {"Start Menu", CSIDL_STARTMENU, 1},
    {"Startup", CSIDL_STARTUP, 1},
    {"Favorites", CSIDL_FAVORITES, 1},
    {"My Music", CSIDL_MYMUSIC, 1},
    {"My Pictures", CSIDL_MYPICTURES, 1},
    {"My Video", CSIDL_MYVIDEO, 1},
    {"SendTo", CSIDL_SENDTO, 1},
    {"Recent", CSIDL_RECENT, 1},
    {"Templates", CSIDL_TEMPLATES, 1},
    {"Common AppData", CSIDL_COMMON_APPDATA, 0},
    {"Common Desktop", CSIDL_COMMON_DESKTOPDIRECTORY, 0},
    {"Common Documents", CSIDL_COMMON_DOCUMENTS, 0},
    {"Common Programs", CSIDL_COMMON_PROGRAMS, 0},
    {"Common Start Menu", CSIDL_COMMON_STARTMENU, 0},
    {"Common Startup", CSIDL_COMMON_STARTUP, 0},
};

static const char *query(HKEY root, const char *key, const char *name, DWORD *type, char *out) {
    HKEY h;
    if (RegOpenKeyExA(root, key, 0, KEY_READ, &h))
        return "no key";
    DWORD n = MAX_PATH;
    LONG r = RegQueryValueExA(h, name, NULL, type, (BYTE *)out, &n);
    RegCloseKey(h);
    return r ? "no value" : NULL;
}

int main(void) {
    const char *sf = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Shell Folders";
    const char *usf = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\User Shell Folders";
    for (size_t i = 0; i < sizeof F / sizeof *F; i++) {
        HKEY root = F[i].user ? HKEY_CURRENT_USER : HKEY_LOCAL_MACHINE;
        char v[MAX_PATH], s[2 * MAX_PATH], p[MAX_PATH];
        DWORD type = 0;
        const char *e = query(root, sf, F[i].name, &type, v);
        if (e) {
            printf("%s: %s\n", F[i].name, e);
            continue;
        }
        shown(v, s);
        int same = SHGetFolderPathA(NULL, F[i].csidl, NULL, SHGFP_TYPE_CURRENT, p) == S_OK && !_stricmp(p, v);
        printf("%s: type %lu %s, %s SHGetFolderPath", F[i].name, type, s, same ? "same as" : "differs from");
        e = query(root, usf, F[i].name, &type, v);
        if (e) {
            printf(", user shell folders %s\n", e);
            continue;
        }
        char x[MAX_PATH];
        ExpandEnvironmentStringsA(v, x, sizeof x);
        shown(x, s);
        printf(", user shell folders type %lu %s\n", type, s);
    }
    return 0;
}
