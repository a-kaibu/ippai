/* libs: -lshell32 -lole32 -luuid */
/* The shell folders installers put things in (Inno Setup's {group} is CSIDL_COMMON_PROGRAMS or
 * CSIDL_PROGRAMS, a per-user install goes to FOLDERID_UserProgramFiles): SHGetFolderPath for the
 * user's and the common Start menu, Programs, Startup, desktops, documents and the rest, and
 * SHGetKnownFolderPath for their FOLDERIDs, each the same folder both ways. Paths are shown
 * from the environment (%USERPROFILE%, %PUBLIC%, %ProgramData%, %APPDATA%, %LOCALAPPDATA%) so
 * that they compare with any Windows user. (sake: misc.rs special_folder, SHGetKnownFolderPath;
 * compared with Windows by tests/winref)
 */
#include <windows.h>
#include <shlobj.h>
#include <knownfolders.h>
#include <stdio.h>
#include <string.h>
#include <wchar.h>

static const char *vars[] = {"APPDATA", "LOCALAPPDATA", "PUBLIC", "ProgramData", "USERPROFILE"};

static void shown(const char *p, char *out) {
    for (int i = 0; i < 5; i++) {
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
    int csidl;
    const char *name;
    const KNOWNFOLDERID *id;
} F[] = {
    {CSIDL_PROGRAMS, "PROGRAMS", &FOLDERID_Programs},
    {CSIDL_COMMON_PROGRAMS, "COMMON_PROGRAMS", &FOLDERID_CommonPrograms},
    {CSIDL_STARTMENU, "STARTMENU", &FOLDERID_StartMenu},
    {CSIDL_COMMON_STARTMENU, "COMMON_STARTMENU", &FOLDERID_CommonStartMenu},
    {CSIDL_STARTUP, "STARTUP", &FOLDERID_Startup},
    {CSIDL_COMMON_STARTUP, "COMMON_STARTUP", &FOLDERID_CommonStartup},
    {CSIDL_DESKTOPDIRECTORY, "DESKTOPDIRECTORY", &FOLDERID_Desktop},
    {CSIDL_COMMON_DESKTOPDIRECTORY, "COMMON_DESKTOPDIRECTORY", &FOLDERID_PublicDesktop},
    {CSIDL_PERSONAL, "PERSONAL", &FOLDERID_Documents},
    {CSIDL_COMMON_DOCUMENTS, "COMMON_DOCUMENTS", &FOLDERID_PublicDocuments},
    {CSIDL_APPDATA, "APPDATA", &FOLDERID_RoamingAppData},
    {CSIDL_LOCAL_APPDATA, "LOCAL_APPDATA", &FOLDERID_LocalAppData},
    {CSIDL_COMMON_APPDATA, "COMMON_APPDATA", &FOLDERID_ProgramData},
    {CSIDL_FAVORITES, "FAVORITES", &FOLDERID_Favorites},
    {CSIDL_SENDTO, "SENDTO", &FOLDERID_SendTo},
    {CSIDL_RECENT, "RECENT", &FOLDERID_Recent},
    {CSIDL_TEMPLATES, "TEMPLATES", &FOLDERID_Templates},
    {CSIDL_MYMUSIC, "MYMUSIC", &FOLDERID_Music},
    {CSIDL_MYVIDEO, "MYVIDEO", &FOLDERID_Videos},
    {CSIDL_MYPICTURES, "MYPICTURES", &FOLDERID_Pictures},
    {CSIDL_PROFILE, "PROFILE", &FOLDERID_Profile},
    {CSIDL_ADMINTOOLS, "ADMINTOOLS", NULL},
    {CSIDL_COMMON_ADMINTOOLS, "COMMON_ADMINTOOLS", NULL},
    {CSIDL_COMMON_TEMPLATES, "COMMON_TEMPLATES", NULL},
    {CSIDL_NETHOOD, "NETHOOD", NULL},
    {CSIDL_PRINTHOOD, "PRINTHOOD", NULL},
};

int main(void) {
    char p[MAX_PATH], s[2 * MAX_PATH];
    for (size_t i = 0; i < sizeof F / sizeof *F; i++) {
        HRESULT hr = SHGetFolderPathA(NULL, F[i].csidl, NULL, SHGFP_TYPE_CURRENT, p);
        if (hr != S_OK) {
            printf("%s: %lx\n", F[i].name, hr);
            continue;
        }
        shown(p, s);
        printf("%s: %s", F[i].name, s);
        if (F[i].id) {
            PWSTR w = NULL;
            hr = SHGetKnownFolderPath(F[i].id, 0, NULL, &w);
            char a[MAX_PATH] = "";
            if (hr == S_OK)
                WideCharToMultiByte(CP_ACP, 0, w, -1, a, sizeof a, NULL, NULL);
            printf(", known folder %s", hr != S_OK ? "failed" : _stricmp(a, p) ? a : "the same");
            CoTaskMemFree(w);
        }
        printf("\n");
    }
    PWSTR w = NULL;
    HRESULT hr = SHGetKnownFolderPath(&FOLDERID_UserProgramFiles, KF_FLAG_CREATE, NULL, &w);
    char a[MAX_PATH] = "";
    if (hr == S_OK)
        WideCharToMultiByte(CP_ACP, 0, w, -1, a, sizeof a, NULL, NULL);
    shown(a, s);
    printf("UserProgramFiles: %s\n", hr == S_OK ? s : "failed");
    CoTaskMemFree(w);
    return 0;
}
