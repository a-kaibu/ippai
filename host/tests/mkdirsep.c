/* libs: -lkernel32 -lshell32 */
/* CreateDirectory with a trailing separator and with parents spelled in another case than the
 * folders on disk, as KiriKiri Z's TVPCreateFolders makes a save folder under Documents
 * ("c:\users\user\documents\ORCSOFT\" lower-cased, each parent with its separator): the folder
 * is made inside the existing parents, a second spelling of an existing folder is
 * ERROR_ALREADY_EXISTS (not a second folder), a file of that name too, and SHCreateDirectoryEx
 * makes the missing parents the same way. (sake: k32.rs make_dir, files.rs resolve_new_dir,
 * misc.rs sh_mkdir; the ORCSOFT KiriKiri Z trial stopped at "Cannot open storage")
 */
#include <windows.h>
#include <shlobj.h>
#include <stdio.h>
#include <string.h>

static char cwd[MAX_PATH];

static void mk(const char *p) {
    SetLastError(0xdead);
    BOOL r = CreateDirectoryA(p, NULL);
    printf("CreateDirectory %s: %d%s", p, r, r ? "\n" : "");
    if (!r) printf(", error %lu\n", GetLastError());
}

/* the entries of dir whose name is `name` in any case */
static int count(const char *dir, const char *name) {
    char pat[MAX_PATH];
    snprintf(pat, sizeof pat, "%s\\*", dir);
    WIN32_FIND_DATAA fd;
    HANDLE h = FindFirstFileA(pat, &fd);
    int n = 0;
    if (h == INVALID_HANDLE_VALUE) return -1;
    do n += !_stricmp(fd.cFileName, name); while (FindNextFileA(h, &fd));
    FindClose(h);
    return n;
}

static void isdir(const char *p) {
    DWORD a = GetFileAttributesA(p);
    printf("%s: %s\n", p, a == INVALID_FILE_ATTRIBUTES ? "missing" : (a & FILE_ATTRIBUTE_DIRECTORY) ? "folder" : "file");
}

int main(void) {
    char full[MAX_PATH], buf[MAX_PATH];
    GetCurrentDirectoryA(sizeof cwd, cwd);
    mk("MkSep");
    mk("MkSep\\Docs");
    mk("mksep\\docs\\Game\\");
    isdir("MkSep\\Docs\\Game");
    mk("MKSEP\\DOCS\\game");
    mk("mksep\\docs\\GAME\\\\");
    mk("mksep/docs/game/");
    printf("spellings of game: %d, of docs: %d\n", count("MkSep\\Docs", "game"), count("MkSep", "docs"));
    /* a missing parent */
    mk("mksep\\nothere\\x\\");
    isdir("MkSep\\nothere");
    /* KiriKiri's way: the full path lower-cased, each level with its separator */
    snprintf(full, sizeof full, "%s\\mksep\\docs\\orc\\", cwd);
    CharLowerA(full + strlen(cwd));
    printf("full ...\\mksep\\docs\\orc\\: %d\n", CreateDirectoryA(full, NULL));
    snprintf(buf, sizeof buf, "%s\\mksep\\docs\\orc\\title\\", cwd);
    printf("full ...\\orc\\title\\: %d\n", CreateDirectoryA(buf, NULL));
    snprintf(buf, sizeof buf, "%s\\mksep\\docs\\orc\\title\\datasc.ksd", cwd);
    HANDLE f = CreateFileA(buf, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    printf("save file in it: %s\n", f == INVALID_HANDLE_VALUE ? "failed" : "made");
    if (f != INVALID_HANDLE_VALUE) CloseHandle(f);
    isdir("MkSep\\Docs\\orc\\title\\datasc.ksd");
    printf("spellings of docs: %d\n", count("MkSep", "docs"));
    /* a file of that name */
    mk("MKSEP\\DOCS\\ORC\\TITLE\\DATASC.KSD");
    /* SHCreateDirectoryEx: missing parents, existing ones in another case, a trailing separator */
    snprintf(buf, sizeof buf, "%s\\MKSEP\\DOCS\\sh\\a\\b\\", cwd);
    printf("SHCreateDirectoryEx ...\\MKSEP\\DOCS\\sh\\a\\b\\: %d\n", SHCreateDirectoryExA(NULL, buf, NULL));
    printf("SHCreateDirectoryEx again: %d\n", SHCreateDirectoryExA(NULL, buf, NULL));
    isdir("MkSep\\Docs\\sh\\a\\b");
    printf("spellings of mksep: %d, of docs: %d\n", count(".", "mksep"), count("MkSep", "docs"));
    /* clean up */
    DeleteFileA("MkSep\\Docs\\orc\\title\\datasc.ksd");
    const char *dirs[] = {"MkSep\\Docs\\sh\\a\\b", "MkSep\\Docs\\sh\\a", "MkSep\\Docs\\sh", "MkSep\\Docs\\orc\\title",
                          "MkSep\\Docs\\orc", "MkSep\\Docs\\Game", "MkSep\\Docs", "MkSep"};
    int ok = 1;
    for (int i = 0; i < 8; i++) ok &= RemoveDirectoryA(dirs[i]) != 0;
    printf("removed: %d\n", ok);
    return 0;
}
