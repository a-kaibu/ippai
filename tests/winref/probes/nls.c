/* kernel32/user32 text: code page 932 and UTF-8 conversions (invalid input, default chars, MB_ERR
 * flags), CompareString orderings, LCMapString (case, width, kana), CharUpper/Lower, lstrcmp, wsprintf. */
#include "../probe.h"

static void hexw(const WCHAR *w, int n) { for (int i = 0; i < n; i++) printf(" %04x", w[i]); }
static void hexa(const char *a, int n) { for (int i = 0; i < n; i++) printf(" %02x", (unsigned char)a[i]); }

static void m2w(UINT cp, DWORD fl, const char *s, int len) {
    WCHAR w[64];
    SetLastError(0);
    int n = MultiByteToWideChar(cp, fl, s, len, w, 64);
    int need = MultiByteToWideChar(cp, fl, s, len, NULL, 0);
    printf("m2w %u %lx [", cp, fl); hexa(s, len < 0 ? (int)strlen(s) + 1 : len);
    printf(" ] -> %d need %d err %lu:", n, need, n ? 0 : GetLastError()); hexw(w, n > 0 ? n : 0); printf("\n");
}

static void w2m(UINT cp, DWORD fl, const WCHAR *w, int len) {
    char a[64];
    BOOL used = -1;
    SetLastError(0);
    int n = WideCharToMultiByte(cp, fl, w, len, a, 64, NULL, cp == CP_UTF8 ? NULL : &used);
    printf("w2m %u %lx [", cp, fl); hexw(w, len < 0 ? (int)wcslen(w) + 1 : len);
    printf(" ] -> %d used %d err %lu:", n, used, n ? 0 : GetLastError()); hexa(a, n > 0 ? n : 0); printf("\n");
}

int main(void) {
    /* Shift-JIS: kana, kanji, half-width kana, NEC/IBM extensions, user-defined area, truncated
     * lead byte, invalid trail, lone trail-range bytes */
    static const char *sj[] = { "\x82\xa0\x88\x9f", "\xb1\xdf", "\x87\x40\xed\x40\xfa\x40", "\xf0\x40",
                                "abc\x82", "\x82\x20", "\x80\xa0\xfd\xfe\xff", "\x81\x5f\x81\x60\x81\x61\x81\x7c", "\\~" };
    for (unsigned i = 0; i < sizeof sj / sizeof *sj; i++) {
        m2w(932, 0, sj[i], (int)strlen(sj[i]));
        m2w(932, MB_ERR_INVALID_CHARS, sj[i], (int)strlen(sj[i]));
    }
    m2w(932, 0, "x", -1);
    static const char *u8[] = { "\xe3\x81\x82", "\xf0\x9f\x98\x80", "\xc0\xaf", "\xed\xa0\x80", "\xe3\x81", "\xff", "a\x80z" };
    for (unsigned i = 0; i < sizeof u8 / sizeof *u8; i++) {
        m2w(CP_UTF8, 0, u8[i], (int)strlen(u8[i]));
        m2w(CP_UTF8, MB_ERR_INVALID_CHARS, u8[i], (int)strlen(u8[i]));
    }
    m2w(1252, 0, "\x80\x81\x8d\x9f\xa0\xff", 6);
    static const WCHAR w1[] = { 0x3042, 0x4e9c, 0xff71, 0x2460, 0x00e9, 0x20ac, 0xd83d, 0xde00, 0x301c, 0xff5e, 0x2225, 0x00a5, 0 };
    for (int i = 0; w1[i]; i++) {
        w2m(932, 0, w1 + i, 1);
        w2m(CP_UTF8, 0, w1 + i, w1[i] == 0xd83d ? 2 : 1);
    }
    w2m(1252, 0, L"\x20ac\x0153\x0100", 3);
    w2m(932, WC_NO_BEST_FIT_CHARS, L"\x00e9", 1);
    w2m(932, 0, L"ab", -1);
    {
        char a[2];
        SetLastError(0);
        int n = WideCharToMultiByte(932, 0, L"\x3042", 1, a, 1, NULL, NULL);
        printf("w2m small %d err %lu\n", n, GetLastError());
    }
    printf("IsDBCSLeadByteEx");
    for (int c = 0x80; c < 0x100; c += 4) printf(" %d", IsDBCSLeadByteEx(932, c) ? 1 : 0);
    printf("\n");
    {
        CPINFO ci;
        GetCPInfo(932, &ci);
        printf("GetCPInfo 932 %u default %02x lead", ci.MaxCharSize, ci.DefaultChar[0]);
        for (int i = 0; i < 12 && ci.LeadByte[i]; i++) printf(" %02x", ci.LeadByte[i]);
        printf("\n");
    }
    /* orderings: word sort puts '-' and '\'' aside; case; kana; numbers */
    static const char *ws[] = { "a", "A", "b", "B", "-a", "a-b", "ab", "co-op", "coop", "a'b", "_a", "1", "10", "2", "",
                                "\x82\xa0", "\x83\x41", "\xb1", "z", "Z", "\xe9", "\xc9" };
    int n = sizeof ws / sizeof *ws;
    for (int i = 0; i < n; i++) {
        printf("cmp %d:", i);
        for (int j = 0; j < n; j++) {
            int c = CompareStringA(0x411, 0, ws[i], -1, ws[j], -1) - 2;
            int ci = CompareStringA(0x411, NORM_IGNORECASE, ws[i], -1, ws[j], -1) - 2;
            int ss = CompareStringA(0x411, SORT_STRINGSORT, ws[i], -1, ws[j], -1) - 2;
            int l = lstrcmpA(ws[i], ws[j]), li = lstrcmpiA(ws[i], ws[j]);
            printf(" %d%d%d%d%d", c + 1, ci + 1, ss + 1, (l > 0) - (l < 0) + 1, (li > 0) - (li < 0) + 1);
        }
        printf("\n");
    }
    printf("CompareStringW kana %d width %d\n",
           CompareStringW(0x411, NORM_IGNOREKANATYPE, L"\x3042", 1, L"\x30a2", 1),
           CompareStringW(0x411, NORM_IGNOREWIDTH, L"A", 1, L"\xff21", 1));
    {
        static const DWORD maps[] = { LCMAP_UPPERCASE, LCMAP_LOWERCASE, LCMAP_HALFWIDTH, LCMAP_FULLWIDTH,
                                      LCMAP_HIRAGANA, LCMAP_KATAKANA, LCMAP_KATAKANA | LCMAP_HALFWIDTH,
                                      LCMAP_HIRAGANA | LCMAP_FULLWIDTH };
        static const WCHAR src[] = { 'a', 'Z', 0x3042, 0x30ac, 0x304c, 0xff76, 0xff9e, 0xff21, 0x3000, ' ', 0x30fc, 0xff70, 0x00e9, 0x0430, 0 };
        for (unsigned i = 0; i < sizeof maps / sizeof *maps; i++) {
            WCHAR d[64];
            int r = LCMapStringW(0x411, maps[i], src, -1, d, 64);
            printf("LCMapStringW %lx -> %d:", maps[i], r); hexw(d, r > 0 ? r : 0); printf("\n");
        }
        char d[64];
        int r = LCMapStringA(0x411, LCMAP_UPPERCASE, "abc\x82\xa0\x82\x81xyz", -1, d, 64);
        printf("LCMapStringA upper %d:", r); hexa(d, r > 0 ? r : 0); printf("\n");
    }
    {
        char b[64] = "abc\x82\x81\x82\x9ax\xe0\xe9";
        CharUpperA(b);
        printf("CharUpperA:"); hexa(b, (int)strlen(b)); printf("\n");
        CharLowerA(b);
        printf("CharLowerA:"); hexa(b, (int)strlen(b)); printf("\n");
        printf("CharNextA %d %d\n", (int)(CharNextA(b + 3) - b), (int)(CharNextExA(932, "\x82\xa0", 0) - (LPCSTR)"\x82\xa0"));
        printf("IsCharAlphaA %d %d IsCharAlphaW %d %d\n", IsCharAlphaA('a'), IsCharAlphaA('1'), IsCharAlphaW(0x3042), IsCharAlphaW(0x4e00));
    }
    {
        char b[256];
        int r = wsprintfA(b, "%d|%5d|%-5d|%05d|%u|%x|%X|%#x|%lx|%c|%s|%.2s|%ls|%%|%i", -42, 7, 7, 7, 4000000000u,
                          255, 255, 255, 0xabcdefL, 'q', "str", "str", L"wide", 9);
        printf("wsprintfA %d %s\n", r, b);
        r = wsprintfA(b, "%I64d %f %e %p", 1ll, 1.0, 1.0, (void *)0);
        printf("wsprintfA unsupported %d %s\n", r, b);
    }
    {
        WORD t[8];
        GetStringTypeW(CT_CTYPE1, L"aZ1 \x3042\xff10!\x4e00", 8, t);
        printf("GetStringTypeW c1"); for (int i = 0; i < 8; i++) printf(" %03x", t[i]); printf("\n");
        GetStringTypeW(CT_CTYPE3, L"aZ1 \x3042\xff10!\x4e00", 8, t);
        printf("GetStringTypeW c3"); for (int i = 0; i < 8; i++) printf(" %04x", t[i]); printf("\n");
    }
    return 0;
}
