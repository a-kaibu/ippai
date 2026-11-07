/* kernel32 profile (ini) files as games keep their settings: Get/WritePrivateProfileString,
 * Int, Section, SectionNames, Struct on a file with odd formatting (spaces, quotes, comments,
 * duplicate keys and sections, case, missing '=' and ']'), the buffer-too-small answers, what
 * deleting a key or section leaves, and the exact bytes written back. Works in "wr-ini". */
#include "../probe.h"

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-76s = %lld err %lu\n", #expr, r_, GetLastError()); } while (0)

static char path[MAX_PATH];

static void dump(void) {
    HANDLE h = CreateFileA(path, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    char b[4096];
    DWORD n = 0;
    ReadFile(h, b, sizeof b, &n, NULL);
    CloseHandle(h);
    printf("file %lu bytes:\n", n);
    for (DWORD i = 0; i < n; i++) {
        unsigned char c = b[i];
        if (c == '\r') printf("\\r");
        else if (c == '\n') printf("\\n\n");
        else if (c < 32 || c >= 127) printf("\\x%02x", c);
        else putchar(c);
    }
    printf("|\n");
}

static void list(const char *what, const char *b, DWORD n) {
    printf("%s %lu [", what, n);
    for (DWORD i = 0; i < n + 1; i++) putchar(b[i] ? b[i] : '|');
    printf("]\n");
}

static void get(const char *sec, const char *key, const char *def, DWORD cap) {
    char b[256];
    memset(b, '#', sizeof b);
    SetLastError(0xdead);
    DWORD n = GetPrivateProfileStringA(sec, key, def, b, cap, path);
    DWORD e = GetLastError();
    printf("get [%s] %s def %s cap %lu: %lu err %lu [", sec ? sec : "(null)", key ? key : "(null)", def ? def : "(null)", cap, n, e);
    for (DWORD i = 0; i < n + 2 && i < cap + 1 && i < sizeof b; i++) putchar(b[i] ? b[i] : '|');
    printf("]\n");
}

int main(void) {
    CreateDirectoryA("wr-ini", NULL);
    GetFullPathNameA("wr-ini\\t.ini", sizeof path, path, NULL);
    const char *text =
        "; comment line\r\n"
        "top=before any section\r\n"
        "[Main]\r\n"
        "Name = Alice  \r\n"
        "quoted=\"  spaced  \"\r\n"
        "single='x'\r\n"
        "half=\"open\r\n"
        "num=42\r\n"
        "neg=-7\r\n"
        "hex=0x1F\r\n"
        "big=99999999999\r\n"
        "lead=  12abc\r\n"
        "empty=\r\n"
        "noequals\r\n"
        "dup=first\r\n"
        "DUP=second\r\n"
        "  indented key = v  \r\n"
        "semi=a;b\r\n"
        "tab=\tt\t\r\n"
        "[ Spaced ]\r\n"
        "k=spaced section\r\n"
        "[main]\r\n"
        "extra=second main\r\n"
        "[Unclosed\r\n"
        "u=1\r\n"
        "[Empty]\r\n"
        "[Bin]\r\n"
        "data=0102030405D2\r\n"
        "[Last]\r\n"
        "nonl=end";
    HANDLE h = CreateFileA(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD w;
    WriteFile(h, text, strlen(text), &w, NULL);
    CloseHandle(h);

    get("Main", "Name", "d", 64);
    get("main", "NAME", "d", 64);
    get("Main", "quoted", "d", 64);
    get("Main", "single", "d", 64);
    get("Main", "half", "d", 64);
    get("Main", "empty", "d", 64);
    get("Main", "noequals", "d", 64);
    get("Main", "dup", "d", 64);
    get("Main", "indented key", "d", 64);
    get("Main", "semi", "d", 64);
    get("Main", "tab", "d", 64);
    get("Main", "extra", "d", 64);
    get("Main", "missing", "  def with spaces  ", 64);
    get("Main", "missing", NULL, 64);
    get("Main", "missing", "", 64);
    get("Spaced", "k", "d", 64);
    get(" Spaced ", "k", "d", 64);
    get("Unclosed", "u", "d", 64);
    get("Last", "nonl", "d", 64);
    get("", "top", "d", 64);
    get("Nope", "x", "default", 64);
    get("Main", "Name", "d", 5);
    get("Main", "Name", "d", 6);
    get("Main", "Name", "d", 1);
    get("Main", "Name", "d", 0);
    get("Main", "missing", "longdefault", 4);
    get("Main", NULL, "d", 256);
    get("Main", NULL, "d", 20);
    get(NULL, NULL, "d", 256);
    get(NULL, NULL, "d", 12);
    get("Empty", NULL, "d", 64);

    const char *ik[] = { "num", "neg", "hex", "big", "lead", "Name", "missing", "empty" };
    for (int i = 0; i < 8; i++) printf("int %s = %d\n", ik[i], GetPrivateProfileIntA("Main", ik[i], 5, path));

    char sb[512];
    memset(sb, '#', sizeof sb);
    DWORD n = GetPrivateProfileSectionA("Main", sb, sizeof sb, path);
    list("section Main", sb, n);
    memset(sb, '#', sizeof sb);
    n = GetPrivateProfileSectionA("Main", sb, 30, path);
    list("section Main cap 30", sb, n);
    memset(sb, '#', sizeof sb);
    n = GetPrivateProfileSectionNamesA(sb, sizeof sb, path);
    list("names", sb, n);
    memset(sb, '#', sizeof sb);
    n = GetPrivateProfileSectionNamesA(sb, 10, path);
    list("names cap 10", sb, n);

    BYTE st[6] = { 0 };
    E(GetPrivateProfileStructA("Bin", "data", st, 5, path));
    E(GetPrivateProfileStructA("Bin", "data", st, 6, path));
    E(GetPrivateProfileStructA("Bin", "data", st, 4, path));
    E(WritePrivateProfileStructA("Bin", "out", (BYTE *)"\x10\x20\xff", 3, path));

    /* writes */
    E(WritePrivateProfileStringA("Main", "num", "43", path));
    E(WritePrivateProfileStringA("main", "NewKey", "  new value  ", path));
    E(WritePrivateProfileStringA("Main", "Name", NULL, path));
    E(WritePrivateProfileStringA("Fresh", "a", "1", path));
    E(WritePrivateProfileStringA("Fresh", "b", "\"q\"", path));
    E(WritePrivateProfileStringA("Unclosed", "u", "2", path));
    E(WritePrivateProfileStringA("Last", "added", "x", path));
    E(WritePrivateProfileStringA("Spaced", NULL, NULL, path));
    E(WritePrivateProfileStringA(NULL, NULL, NULL, path));
    E(WritePrivateProfileSectionA("Sec", "k1=v1\0k2=v2\0", path));
    E(WritePrivateProfileSectionA("Main", "only=this\0", path));
    dump();

    /* a file that doesn't exist yet: created by the first write */
    char p2[MAX_PATH];
    GetFullPathNameA("wr-ini\\new.ini", sizeof p2, p2, NULL);
    DeleteFileA(p2);
    char b[16];
    SetLastError(0xdead);
    DWORD r = GetPrivateProfileStringA("a", "b", "dflt", b, sizeof b, p2);
    printf("missing file: %lu err %lu [%s]\n", r, GetLastError(), b);
    E(WritePrivateProfileStringA("S", "k", "v", p2));
    strcpy(path, p2);
    dump();
    E(WritePrivateProfileStringA("S", "k", NULL, p2));
    dump();
    E(WritePrivateProfileStringA("S", NULL, NULL, p2));
    dump();
    E(GetPrivateProfileIntA("S", "k", -3, p2));
    return 0;
}
