/* shell32's SHFileOperation without UI (FOF_NO_UI), as installers use it (NSIS's CopyFiles,
 * electron-builder's installer copying the unpacked app, #183): copy of a wildcard into a missing
 * folder, FOF_FILESONLY, one file to a new name and into a folder, a folder to a new name, move,
 * rename, a missing source, an empty list, delete with wildcards and of a whole tree. Each
 * result is the return value and which files are there afterwards.
 * libs: -lshell32 */
#include "../probe.h"
#include <shellapi.h>

static char base[MAX_PATH];

static void put(const char *rel, const char *data) {
    char p[MAX_PATH];
    snprintf(p, sizeof p, "%s\\%s", base, rel);
    HANDLE h = CreateFileA(p, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD n;
    WriteFile(h, data, (DWORD)strlen(data), &n, NULL);
    CloseHandle(h);
}

/* a double-NUL list of base-relative paths separated by '|' */
static char *list(const char *rels, char *out) {
    char *o = out;
    while (*rels) {
        const char *e = strchr(rels, '|');
        size_t n = e ? (size_t)(e - rels) : strlen(rels);
        o += sprintf(o, "%s\\%.*s", base, (int)n, rels) + 1;
        rels += n + (e ? 1 : 0);
    }
    *o = 0;
    return out;
}

static void op(const char *what, UINT func, const char *from, const char *to, FILEOP_FLAGS flags) {
    char f[4096], t[4096];
    SHFILEOPSTRUCTA s = { 0 };
    s.wFunc = func;
    s.pFrom = from ? list(from, f) : "\0";
    s.pTo = to ? list(to, t) : NULL;
    s.fFlags = flags | FOF_NO_UI;
    s.fAnyOperationsAborted = 7;
    int r = SHFileOperationA(&s);
    printf("%s: %#x aborted %d\n", what, r, s.fAnyOperationsAborted);
}

static void there(const char *rel) {
    char p[MAX_PATH];
    snprintf(p, sizeof p, "%s\\%s", base, rel);
    DWORD a = GetFileAttributesA(p);
    printf("  %s: %s\n", rel, a == INVALID_FILE_ATTRIBUTES ? "missing" : (a & FILE_ATTRIBUTE_DIRECTORY) ? "folder" : "file");
}

int main(void) {
    char tmp[MAX_PATH], p[MAX_PATH];
    GetTempPathA(sizeof tmp, tmp);
    snprintf(base, sizeof base, "%sshfileop-%lu", tmp, GetCurrentProcessId());
    CreateDirectoryA(base, NULL);
    snprintf(p, sizeof p, "%s\\src", base);
    CreateDirectoryA(p, NULL);
    snprintf(p, sizeof p, "%s\\src\\sub", base);
    CreateDirectoryA(p, NULL);
    put("src\\a.txt", "a");
    put("src\\c.txt", "c");
    put("src\\sub\\b.dat", "b");

    op("copy src\\*.* to missing dst", FO_COPY, "src\\*.*", "dst", 0);
    there("dst\\a.txt");
    there("dst\\sub\\b.dat");
    op("copy src\\* FILESONLY to files", FO_COPY, "src\\*", "files", FOF_FILESONLY);
    there("files\\a.txt");
    there("files\\sub");
    op("copy a.txt to renamed.txt", FO_COPY, "src\\a.txt", "renamed.txt", 0);
    there("renamed.txt");
    op("copy c.txt into files", FO_COPY, "src\\c.txt", "files", 0);
    there("files\\c.txt");
    op("copy src to copy", FO_COPY, "src", "copy", 0);
    there("copy\\sub\\b.dat");
    op("copy two files to missing two", FO_COPY, "src\\a.txt|src\\c.txt", "two", 0);
    there("two\\a.txt");
    there("two\\c.txt");
    op("move renamed.txt to moved.txt", FO_MOVE, "renamed.txt", "moved.txt", 0);
    there("renamed.txt");
    there("moved.txt");
    op("rename moved.txt to r.txt", FO_RENAME, "moved.txt", "r.txt", 0);
    there("moved.txt");
    there("r.txt");
    op("delete files\\*.txt", FO_DELETE, "files\\*.txt", NULL, 0);
    there("files\\a.txt");
    there("files");
    op("delete whole copy", FO_DELETE, "copy", NULL, 0);
    there("copy");
    op("copy a missing file", FO_COPY, "nothing.txt", "files", 0);
    op("copy an empty list", FO_COPY, "", "files", 0);
    op("delete the rest", FO_DELETE, "src|dst|files|two|r.txt", NULL, 0);
    there("src");
    there("two");
    RemoveDirectoryA(base);
    return 0;
}
