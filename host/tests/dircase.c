/* New names under a folder spelled in another case, and CreateDirectory with a trailing
 * backslash (YU-RIS: iegami03_trial.exe lowers the case of its own path, then
 * CreateDirectoryA("...\\(実況ok)\\save\\") for the folder "(実況OK)"; on a case-sensitive
 * host sake did not find the parent and the game stopped on a script error). */
#include <stdio.h>
#include <windows.h>

static char base[MAX_PATH];

/* CreateDirectory of %TEMP%\rel, printed by rel (the temporary folder differs per system) */
static void mk(const char *rel)
{
    char p[MAX_PATH * 2];
    snprintf(p, sizeof p, "%s%s", base, rel);
    BOOL r = CreateDirectoryA(p, NULL);
    printf("CreateDirectory(%s): %d, error %lu\n", rel, r, r ? 0 : GetLastError());
}

int main(void)
{
    char p[MAX_PATH * 2];
    GetTempPathA(MAX_PATH, base);
    snprintf(p, sizeof p, "%sDirCaseOK", base);
    RemoveDirectoryA(p);
    mk("DirCaseOK");
    /* the parent lower-cased: with and without trailing backslashes */
    mk("dircaseok\\save\\");
    mk("DIRCASEOK\\save");
    mk("dircaseok\\two\\\\");
    /* a file in it through the other spelling, then found through the real one */
    snprintf(p, sizeof p, "%sdircaseok\\SAVE\\gsd.sd", base);
    HANDLE h = CreateFileA(p, GENERIC_WRITE, 0, NULL, CREATE_NEW, 0x80, NULL);
    printf("CreateFile CREATE_NEW: %s\n", h != INVALID_HANDLE_VALUE ? "ok" : "failed");
    if (h != INVALID_HANDLE_VALUE) CloseHandle(h);
    snprintf(p, sizeof p, "%sDirCaseOK\\save\\gsd.sd", base);
    printf("attributes: %s\n", GetFileAttributesA(p) != INVALID_FILE_ATTRIBUTES ? "found" : "missing");
    /* the parent missing: path not found */
    mk("dircaseok\\none\\x\\");
    snprintf(p, sizeof p, "%sDirCaseOK\\save\\gsd.sd", base); DeleteFileA(p);
    snprintf(p, sizeof p, "%sDirCaseOK\\save", base); RemoveDirectoryA(p);
    snprintf(p, sizeof p, "%sDirCaseOK\\two", base); RemoveDirectoryA(p);
    snprintf(p, sizeof p, "%sDirCaseOK", base);
    printf("cleaned: %d\n", RemoveDirectoryA(p));
    return 0;
}
