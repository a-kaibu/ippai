/* libs: -lkernel32 */
/* SearchPath as Inno Setup's preprocessor calls it, with a full path as the file name and a
 * search path beside it: a name with a path of its own ("C:\...", ".\x", "..\x") is looked for
 * there only, first as given and then with the extension; a bare name is looked for in each
 * directory of the path; the file part, the length asked for when the buffer is short, and
 * the errors for an empty name and for a file that is not there.
 * (sake: k32.rs search_path; compared with Windows by tests/winref)
 */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static char dir[MAX_PATH];

static void show(const char *what, const char *path, const char *name, const char *ext) {
    char buf[MAX_PATH] = "", *part = NULL;
    SetLastError(0xdead);
    DWORD r = SearchPathA(path, name, ext, sizeof buf, buf, &part);
    DWORD e = GetLastError();
    if (!r) {
        printf("%s: 0, error %lu\n", what, e);
        return;
    }
    size_t dl = strlen(dir);
    const char *shown = _strnicmp(buf, dir, dl) ? buf : buf + dl;
    printf("%s: %s%s (length %s), file part %s\n", what, shown == buf ? "" : "<dir>", shown, r == strlen(buf) ? "ok" : "wrong", part ? part : "(none)");
}

int main(void) {
    char tmp[MAX_PATH];
    GetTempPathA(sizeof tmp, tmp);
    sprintf(dir, "%ssakesearch%lu", tmp, GetCurrentProcessId());
    CreateDirectoryA(dir, NULL);
    char file[MAX_PATH], noext[MAX_PATH], sub[MAX_PATH];
    sprintf(file, "%s\\Builtins.iss", dir);
    sprintf(noext, "%s\\Builtins", dir);
    sprintf(sub, "%s\\sub", dir);
    CreateDirectoryA(sub, NULL);
    HANDLE h = CreateFileA(file, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    CloseHandle(h);

    show("full name, no path", NULL, file, NULL);
    show("full name, another path", "C:\\nowhere;C:\\Windows", file, NULL);
    show("full name without the extension, .iss", "C:\\nowhere", noext, ".iss");
    show("full name of nothing", dir, "C:\\nowhere\\Builtins.iss", NULL);
    show("bare name in the path", dir, "Builtins.iss", NULL);
    show("bare name not in the path", "C:\\nowhere;", "Builtins.iss", NULL);
    char two[2 * MAX_PATH];
    sprintf(two, "C:\\nowhere;%s", dir);
    show("bare name in the second directory", two, "Builtins.iss", NULL);
    show("bare name and extension", dir, "Builtins", ".iss");
    show("extension not added to a name with one", dir, "Builtins.x", ".iss");
    SetCurrentDirectoryA(sub);
    show(".\\ name with a path", dir, ".\\Builtins.iss", NULL);
    show("..\\ name with a path", "C:\\nowhere", "..\\Builtins.iss", NULL);
    show("empty name", dir, "", NULL);
    show("spaces", dir, "   ", NULL);

    char small[4];
    char *part = (char *)1;
    DWORD r = SearchPathA(NULL, file, NULL, sizeof small, small, &part);
    printf("short buffer: asks for the length with the NUL: %s\n", r == strlen(file) + 1 ? "yes" : "no");

    SetCurrentDirectoryA(tmp);
    DeleteFileA(file);
    RemoveDirectoryA(sub);
    RemoveDirectoryA(dir);
    return 0;
}
