/* File names with characters no name can have (? * < > | "): the file functions fail with
 * ERROR_INVALID_NAME before looking for the file, wherever in the path the character is, but
 * FindFirstFile takes wildcards in the last part as its pattern. The MSLU loader (unicows.lib)
 * tells NT from 9x by GetFileAttributesW(L"???.???") giving ERROR_INVALID_NAME (light's
 * malie.exe, しょぱん！). Runs in a scratch directory "wr-badnames" under the current one. */
#include "../probe.h"

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-64s = %lld err %lu\n", #expr, r_, GetLastError()); } while (0)
#define H(expr) do { SetLastError(0xdead); HANDLE h_ = (expr); printf("%-64s = %s err %lu\n", #expr, h_ == INVALID_HANDLE_VALUE ? "invalid" : "handle", GetLastError()); if (h_ != INVALID_HANDLE_VALUE) CloseHandle(h_); } while (0)
#define F(pat) do { WIN32_FIND_DATAA fd; SetLastError(0xdead); HANDLE h_ = FindFirstFileA(pat, &fd); printf("find %-59s = %s err %lu\n", pat, h_ == INVALID_HANDLE_VALUE ? "invalid" : fd.cFileName, GetLastError()); if (h_ != INVALID_HANDLE_VALUE) FindClose(h_); } while (0)

int main(void) {
    CreateDirectoryA("wr-badnames", NULL);
    SetCurrentDirectoryA("wr-badnames");
    HANDLE t = CreateFileA("a.txt", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    CloseHandle(t);
    E(GetFileAttributesW(L"???.???"));
    E(GetFileAttributesA("???.???"));
    E(GetFileAttributesA("a?.txt"));
    E(GetFileAttributesA("a*"));
    E(GetFileAttributesA("a<b"));
    E(GetFileAttributesA("a>b"));
    E(GetFileAttributesA("a|b"));
    E(GetFileAttributesA("a\"b"));
    E(GetFileAttributesA("no?dir\\a.txt"));
    E(GetFileAttributesA("missing\\a?.txt"));
    E(GetFileAttributesA("a.txt"));
    E(GetFileAttributesA("missing.txt"));
    WIN32_FILE_ATTRIBUTE_DATA d;
    E(GetFileAttributesExA("???.???", GetFileExInfoStandard, &d));
    E(GetFileAttributesExW(L"a*.txt", GetFileExInfoStandard, &d));
    H(CreateFileA("???.???", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL));
    H(CreateFileA("new?.txt", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL));
    H(CreateFileA("new*.txt", GENERIC_WRITE, 0, NULL, CREATE_NEW, 0, NULL));
    H(CreateFileA("new|.txt", GENERIC_WRITE, 0, NULL, OPEN_ALWAYS, 0, NULL));
    H(CreateFileW(L"a?.txt", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL));
    E(DeleteFileA("a?.txt"));
    E(MoveFileA("a?.txt", "b.txt"));
    E(CopyFileA("a*.txt", "b.txt", FALSE));
    E(SetFileAttributesA("a?.txt", FILE_ATTRIBUTE_NORMAL));
    F("a?.txt");
    F("*.zzz");
    F("no?dir\\*");
    F("missing\\*.txt");
    E(DeleteFileA("a.txt"));
    SetCurrentDirectoryA("..");
    E(RemoveDirectoryA("wr-badnames"));
    return 0;
}
