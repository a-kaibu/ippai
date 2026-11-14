/* CompareStringW's levels in Japanese: which difference decides once another is ignored. Every
 * pair of the strings below under eight flag sets (none, IGNORECASE, IGNORENONSPACE,
 * IGNOREWIDTH, IGNOREKANATYPE, the last three and case together, IGNORESYMBOLS,
 * SORT_STRINGSORT); one digit per flag set, 0 less, 1 equal, 2 greater. Single characters
 * where case, width, kana kind and marks meet (è against Ｅ, Z against ž), then two-character
 * strings where an earlier character's case meets a later one's width or kana kind (Κﾆ against
 * κニ). sake's collate::compare answers all of them (collate_tests.rs reads this golden). */
#include "../probe.h"

static const DWORD flags[] = { 0, NORM_IGNORECASE, NORM_IGNORENONSPACE, NORM_IGNOREWIDTH, NORM_IGNOREKANATYPE,
                               NORM_IGNORECASE | NORM_IGNOREKANATYPE | NORM_IGNOREWIDTH, NORM_IGNORESYMBOLS,
                               SORT_STRINGSORT };

static void table(const char *name, WCHAR (*s)[3], int n) {
    for (int i = 0; i < n; i++) {
        printf("%s str %d:", name, i);
        for (int k = 0; s[i][k]; k++) printf(" %04x", s[i][k]);
        printf("\n");
    }
    for (int i = 0; i < n; i++) {
        printf("%s row %d:", name, i);
        for (int j = 0; j < n; j++) {
            printf(" ");
            for (unsigned f = 0; f < sizeof flags / sizeof *flags; f++)
                printf("%d", CompareStringW(0x411, flags[f], s[i], -1, s[j], -1) - 1);
        }
        printf("\n");
    }
}

int main(void) {
    static const WCHAR one[] = { 'a', 'A', 'e', 'E', 0xe8, 0xc8, 0xe9, 0xc9, 0xff45, 0xff25, 'z', 'Z', 0x17e, 0x17d,
                                 0xff5a, 0xff3a, 0x3ba, 0x39a, 0x3042, 0x30a2, 0xff71, 0x304b, 0x304c, 0x30ab,
                                 0x30ac, 0xff76, 0x3041, 0x30a1, 0xff67, 0x30fc, '1', 0xff11, '-', '\'', '_', ' ',
                                 0x3000, '.' };
    enum { N1 = sizeof one / sizeof *one };
    static WCHAR s1[N1][3];
    for (int i = 0; i < N1; i++) s1[i][0] = one[i];
    table("one", s1, N1);
    static const WCHAR two[] = { 'a', 'A', 0xff21, 0xe9, 0xc9, 0x3ba, 0x39a, 0x30a2, 0xff71, 0x3042, 0x30cb, 0xff86 };
    enum { N2 = sizeof two / sizeof *two };
    static WCHAR s2[N2 * N2][3];
    for (int i = 0; i < N2 * N2; i++) { s2[i][0] = two[i / N2]; s2[i][1] = two[i % N2]; }
    table("two", s2, N2 * N2);
    return 0;
}
