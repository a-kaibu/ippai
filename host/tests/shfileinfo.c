/* libs: -lshell32 -lshlwapi */
/* SHGetFileInfo as installers (NSIS's directory page) ask it: a folder, a file, a program,
 * a file that is not there (alone and with SHGFI_USEFILEATTRIBUTES), SHGFI_EXETYPE of a GUI
 * program and of a text file; and IsOS for every OS_ value. (sake: misc.rs
 * sh_get_file_info, IsOS; compared with Windows by tests/winref)
 */
#include <windows.h>
#include <shellapi.h>
#include <shlwapi.h>
#include <shlobj.h>
#include <stdio.h>

static char self_name[MAX_PATH];

static void info(const char *what, const char *path, DWORD attrs, UINT flags) {
    SHFILEINFOA fi;
    memset(&fi, 0xcc, sizeof fi);
    DWORD_PTR r = SHGetFileInfoA(path, attrs, &fi, sizeof fi, flags | SHGFI_DISPLAYNAME | SHGFI_ATTRIBUTES);
    if (!r) {
        printf("%s: 0\n", what);
        return;
    }
    printf("%s: ok \"%s\" folder %d filesystem %d\n", what, strcmp(fi.szDisplayName, self_name) ? fi.szDisplayName : "<self>", !!(fi.dwAttributes & SFGAO_FOLDER), !!(fi.dwAttributes & SFGAO_FILESYSTEM));
}

int main(void) {
    char self[MAX_PATH], txt[MAX_PATH], win[MAX_PATH];
    GetModuleFileNameA(NULL, self, sizeof self);
    /* (shown without ".exe": Explorer hides the extensions of known types) */
    strcpy(self_name, strrchr(self, '\\') + 1);
    if (strrchr(self_name, '.')) *strrchr(self_name, '.') = 0;
    GetWindowsDirectoryA(win, sizeof win);
    GetTempPathA(sizeof txt, txt);
    strcat(txt, "probe-text.txt");
    HANDLE h = CreateFileA(txt, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    WriteFile(h, "x", 1, &(DWORD){0}, NULL);
    CloseHandle(h);
    info("windows folder", win, 0, 0);
    info("text file", txt, 0, 0);
    info("program", self, 0, 0);
    info("missing", "C:\\nowhere\\none.bmp", 0, 0);
    info("missing, file attributes", "C:\\nowhere\\none.bmp", FILE_ATTRIBUTE_NORMAL, SHGFI_USEFILEATTRIBUTES);
    info("missing folder, file attributes", "C:\\nowhere\\Folder", FILE_ATTRIBUTE_DIRECTORY, SHGFI_USEFILEATTRIBUTES);
    SHFILEINFOA fi;
    DWORD_PTR il = SHGetFileInfoA(win, 0, &fi, sizeof fi, SHGFI_SYSICONINDEX | SHGFI_SMALLICON);
    DWORD_PTR il2 = SHGetFileInfoA(txt, 0, &fi, sizeof fi, SHGFI_SYSICONINDEX | SHGFI_SMALLICON);
    printf("system image list: %d, same for both %d\n", il != 0, il == il2);
    printf("exe type, GUI: %d\n", (int)(SHGetFileInfoA(self, 0, NULL, 0, SHGFI_EXETYPE) & 0xffff) == 0x4550);
    printf("exe type, text: %lx\n", (unsigned long)SHGetFileInfoA(txt, 0, NULL, 0, SHGFI_EXETYPE));
    DeleteFileA(txt);
    printf("IsOS:");
    for (DWORD os = 0; os <= 36; os++)
        if (os != 30 && os != 28)
            printf(" %lu=%d", os, IsOS(os) ? 1 : 0);
    printf("\n");
    return 0;
}
