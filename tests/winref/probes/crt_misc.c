/* msvcrt: rand's sequence (games use it for gameplay), qsort's order of equal keys, ctype and
 * the multibyte (code page 932) functions, string helpers, strftime on a fixed date. */
#include "../probe.h"
#include <ctype.h>
#include <mbstring.h>
#include <mbctype.h>
#include <locale.h>
#include <time.h>

typedef struct { int key, id; } Pair;
static int cmp(const void *a, const void *b) { return ((const Pair *)a)->key - ((const Pair *)b)->key; }

int main(void) {
    srand(1);
    printf("rand");
    for (int i = 0; i < 16; i++) printf(" %d", rand());
    srand(12345);
    printf("\nrand12345");
    for (int i = 0; i < 8; i++) printf(" %d", rand());
    printf("\nRAND_MAX %d\n", RAND_MAX);

    Pair p[40];
    for (int i = 0; i < 40; i++) { p[i].key = (i * 7) % 5; p[i].id = i; }
    qsort(p, 40, sizeof *p, cmp);
    printf("qsort");
    for (int i = 0; i < 40; i++) printf(" %d", p[i].id);
    printf("\n");
    int keys[] = { 1, 3, 5, 7, 9 }, k = 7;
    int *f = bsearch(&k, keys, 5, sizeof(int), (int (*)(const void *, const void *))cmp);
    printf("bsearch %d\n", f ? (int)(f - keys) : -1);

    /* ctype in the "C" locale, then the 932 multibyte tables */
    for (int c = 0; c < 256; c += 32) {
        printf("ctype %3d", c);
        for (int i = c; i < c + 32; i++)
            printf(" %x", (isalpha(i) ? 1 : 0) | (isdigit(i) ? 2 : 0) | (isspace(i) ? 4 : 0) | (ispunct(i) ? 8 : 0)
                   | (isupper(i) ? 16 : 0) | (iscntrl(i) ? 32 : 0) | (isprint(i) ? 64 : 0));
        printf(" | %d %d\n", toupper(c + 1), tolower(c + 1));
    }
    printf("setlocale %s\n", setlocale(LC_ALL, NULL));
    int setmb = _setmbcp(932); /* (one call a statement: gcc evaluates arguments right to left) */
    printf("_setmbcp %d getmbcp %d\n", setmb, _getmbcp());
    printf("lead");
    for (int c = 0x80; c < 0x100; c += 8) printf(" %d", _ismbblead(c) ? 1 : 0);
    printf("\n");
    const unsigned char sj[] = "\x82\xa0\x82\xa2" "abc\x83\x41\x81\x40" "X";  /* あいabcア　X */
    printf("_mbslen %u strlen %u\n", (unsigned)_mbslen(sj), (unsigned)strlen((const char *)sj));
    printf("_mbschr %d\n", (int)(_mbschr(sj, 'c') - sj));
    printf("_mbsrchr %d\n", (int)(_mbsrchr(sj, 0x8341) - sj));
    printf("_mbsinc %d %d\n", (int)(_mbsinc(sj) - sj), (int)(_mbsinc(sj + 4) - sj));
    printf("_mbsnextc %#x _ismbcspace %d\n", _mbsnextc(sj + 9), _ismbcspace(0x8140));
    printf("_mbctohira %#x _mbctokata %#x _mbcjistojms %#x _mbcjmstojis %#x\n", _mbctohira(0x8341),
           _mbctokata(0x82a0), _mbcjistojms(0x2422), _mbcjmstojis(0x82a0));
    printf("_mbsicmp %d _mbscmp %d\n", _mbsicmp((const unsigned char *)"ABC", (const unsigned char *)"abd"),
           _mbscmp(sj, (const unsigned char *)"\x82\xa0"));
    {
        unsigned char b[32];
        strcpy((char *)b, (const char *)sj);
        _mbsupr(b);
        printf("_mbsupr");
        for (int i = 0; b[i]; i++) printf(" %02x", b[i]);
        printf("\n");
    }
    printf("_stricmp %d %d _strnicmp %d strcmp %d\n", _stricmp("Hello", "hello"), _stricmp("a_", "A["),
           _strnicmp("abcX", "ABCY", 3), strcmp("a", "b"));
    {
        char b[32] = "MiXeD 123";
        printf("_strlwr %s ", _strlwr(b));
        printf("_strupr %s ", _strupr(b));
        printf("_strrev %s\n", _strrev(b));
        char t[] = "a,b;;c,";
        printf("strtok");
        for (char *s = strtok(t, ",;"); s; s = strtok(NULL, ",;")) printf(" [%s]", s);
        printf("\nstrspn %u strcspn %u strpbrk %s strstr %s\n", (unsigned)strspn("abcde", "cba"),
               (unsigned)strcspn("abcde", "dx"), strpbrk("hello", "lo"), strstr("haystack", "st"));
    }
    {
        struct tm t = { 5, 4, 3, 2, 0, 99, 6, 1, 0 }; /* 1999-01-02 03:04:05 Saturday */
        static const char *fm[] = { "%Y-%m-%d %H:%M:%S", "%a %A %b %B", "%c", "%x %X", "%j %U %W %w", "%I %p %y %%" };
        for (unsigned i = 0; i < sizeof fm / sizeof *fm; i++) {
            char b[128];
            size_t n = strftime(b, sizeof b, fm[i], &t);
            printf("strftime '%s' %u %s\n", fm[i], (unsigned)n, b);
        }
        time_t z = 86400 * 365;
        struct tm *g = gmtime(&z);
        printf("gmtime %d-%d-%d %d:%d wday %d yday %d\n", g->tm_year, g->tm_mon, g->tm_mday, g->tm_hour, g->tm_min, g->tm_wday, g->tm_yday);
        printf("asctime %s", asctime(g));
    }
    return 0;
}
