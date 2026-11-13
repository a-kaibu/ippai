/* libs: -lcomdlg32 */
/* GetFileTitleA/W as an LZH self-extractor (LZHSFX32) calls them, Wine's cases, short buffers,
 * and a Shift-JIS name whose trail byte is 0x5c ('表' = 95 5c): not a separator. Not compared
 * with Windows by tests/winref: there the answer drops a known extension when Explorer hides
 * them, a per-user setting. (sake: misc.rs file_title) */
#include <windows.h>
#include <commdlg.h>
#include <stdio.h>

static void esc(const char *s) {
    for (const unsigned char *q = (const unsigned char *)s; *q; q++) printf(*q >= 0x80 ? "\\x%02x" : "%c", *q);
}

static void a(const char *p, WORD cap) {
    char out[64] = "untouched";
    short r = GetFileTitleA(p, out, cap);
    printf("A [");
    esc(p);
    printf("] cap %u: %d", cap, r);
    if (r == 0) {
        printf(" \"");
        esc(out);
        printf("\"");
    }
    printf("\n");
}

static void w(const WCHAR *p, WORD cap) {
    WCHAR out[64] = L"untouched";
    short r = GetFileTitleW(p, out, cap);
    printf("W cap %u: %d", cap, r);
    if (r == 0) {
        printf(" \"");
        for (const WCHAR *q = out; *q; q++) printf(*q >= 0x80 ? "\\u%04x" : "%c", *q);
        printf("\"");
    }
    printf("\n");
}

int main(void) {
    a("C:\\dir\\file.lzh", 64);
    a("file.lzh", 64);
    a("c:file", 64);
    a("a/b/c.lzh", 64);
    a("C:\\dir\\", 64);
    a("C:", 64);
    a("", 64);
    a("C:\\*.lzh", 64);
    a("C:\\[x].lzh", 64);
    a("C:\\dir\\file.lzh", 8);
    a("C:\\dir\\file.lzh", 9);
    a("C:\\dir\\\x95\x5c\x8e\xa6.lzh", 64);
    a("C:\\\x95\x5c\\x.lzh", 64);
    w(L"C:\\dir\\\x8868\x793a.lzh", 64);
    w(L"C:\\dir\\\x8868\x793a.lzh", 3);
    w(L"C:\\dir\\", 64);
    return 0;
}
