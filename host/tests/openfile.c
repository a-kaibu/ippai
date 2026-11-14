/* kernel32 OpenFile, the 16-bit way to open a file: OF_PARSE fills the OFSTRUCT only (the full
 * path, 0 back), OF_CREATE makes the file, OF_EXIST answers 1 and closes it, OF_READ opens it
 * for _hread, a bare name is found in the exe's folder when the current one lacks it, OF_REOPEN
 * takes the name from the OFSTRUCT, a missing file is HFILE_ERROR with error 2 in nErrCode, and
 * OF_DELETE removes it. Crazies (ダンシング・クレイジーズ体験版)' IPF33.DLL reads its pictures with
 * OpenFile + _hread. The expected lines are Windows' answers; x86 and x86-64.
 * libs: -nostdlib -luser32 -lkernel32 */
#include <windows.h>

static void out(const char *f, ...)
{
    char b[512];
    DWORD n;
    va_list a;
    va_start(a, f);
    n = wvsprintfA(b, f, a);
    va_end(a);
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), b, n, &n, NULL);
}

/* (the path only when it was found: a failure leaves szPathName as it was) */
static void show(const char *what, HFILE h, const OFSTRUCT *of, const char *want)
{
    out("%s: %s, cBytes %d, nErrCode %d", what,
        h == HFILE_ERROR ? "HFILE_ERROR" : h == 0 ? "0" : h == 1 ? "1" : "a handle", of->cBytes, of->nErrCode);
    if (want) out(", path %s", lstrcmpiA(of->szPathName, want) == 0 ? "full" : of->szPathName);
    out("\n");
}

void mainCRTStartup(void)
{
    char dir[MAX_PATH], full[MAX_PATH], exe[MAX_PATH], bare[MAX_PATH], buf[16];
    OFSTRUCT of;
    HFILE h;
    long n;
    GetTempPathA(MAX_PATH, dir);
    lstrcatA(dir, "sake-openfile");
    CreateDirectoryA(dir, NULL);
    SetCurrentDirectoryA(dir);
    GetFullPathNameA("of.dat", MAX_PATH, full, NULL);

    h = OpenFile("of.dat", &of, OF_PARSE);
    show("parse", h, &of, full);
    h = OpenFile("of.dat", &of, OF_EXIST);
    show("exist, missing", h, &of, NULL);
    h = OpenFile("of.dat", &of, OF_CREATE | OF_WRITE);
    show("create", h, &of, NULL);
    _lwrite(h, "pictures", 8);
    _lclose(h);
    h = OpenFile("of.dat", &of, OF_EXIST);
    show("exist", h, &of, full);
    h = OpenFile(full, &of, OF_READ);
    show("read", h, &of, full);
    n = _hread(h, buf, sizeof buf);
    buf[n > 0 ? n : 0] = 0;
    out("_hread: %d \"%s\"\n", (int)n, buf);
    _lclose(h);
    h = OpenFile(NULL, &of, OF_REOPEN | OF_READ);
    show("reopen", h, &of, full);
    _lclose(h);

    /* a bare name the current folder lacks: the exe's folder */
    GetModuleFileNameA(NULL, exe, MAX_PATH);
    lstrcpyA(bare, exe);
    {
        char *s = bare + lstrlenA(bare);
        while (s > bare && s[-1] != '\\') s--;
        lstrcpyA(bare, s);
    }
    h = OpenFile(bare, &of, OF_READ);
    show("bare name of the exe", h, &of, exe);
    _lclose(h);

    h = OpenFile("of.dat", &of, OF_DELETE);
    show("delete", h, &of, full);
    h = OpenFile("of.dat", &of, OF_READ);
    show("read, deleted", h, &of, NULL);
    SetCurrentDirectoryA("..");
    RemoveDirectoryA(dir);
    ExitProcess(0);
}
