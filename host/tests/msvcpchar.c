/* x64 msvcp140 ctype<char>: public members must dispatch to custom facets too.
 * Runs as a real PE on sake and as a winref probe on Windows. No MSVC headers needed.
 * The vtable layout is MSVC's ctype<char>, including facet's three base slots. */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static int checks, failures;
#define CHECK(x) do { ++checks; if (!(x)) { ++failures; printf("FAIL line %d: %s\n", __LINE__, #x); } } while (0)
typedef char (*map_fn)(void *, char);
typedef const char *(*map_range_fn)(void *, char *, const char *);
typedef char (*narrow_fn)(void *, char, char);
typedef const char *(*copy_fn)(void *, const char *, const char *, char *);
typedef const char *(*narrow_range_fn)(void *, const char *, const char *, char, char *);
typedef size_t (*getcat_fn)(void **, const void *);
typedef const void *(*classic_fn)(void);
static void *expected_this;
static char *expected_first, *expected_last, *expected_dest;
static int called;
static char custom_upper(void *self, char ch) {
    CHECK(self == expected_this && (unsigned char)ch == 0xe3); called |= 1; return (char)0x91;
}
static const char *custom_upper_range(void *self, char *first, const char *last) {
    CHECK(self == expected_this && first == expected_first && last == expected_last); called |= 2; return first + 1;
}
static char custom_widen(void *self, char ch) {
    CHECK(self == expected_this && (unsigned char)ch == 0xe3); called |= 4; return (char)0x92;
}
static const char *custom_widen_range(void *self, const char *first, const char *last, char *dest) {
    CHECK(self == expected_this && first == expected_first && last == expected_last && dest == expected_dest); called |= 8; return first + 2;
}
static char custom_narrow(void *self, char ch, char dflt) {
    CHECK(self == expected_this && (unsigned char)ch == 0xe3 && (unsigned char)dflt == 0x87); called |= 16; return dflt;
}
static const char *custom_narrow_range(void *self, const char *first, const char *last, char dflt, char *dest) {
    CHECK(self == expected_this && first == expected_first && last == expected_last && dest == expected_dest && (unsigned char)dflt == 0x87); called |= 32; return first + 3;
}
int main(void) {
    HMODULE dll = LoadLibraryA("msvcp140.dll");
    if (!dll) { puts("missing msvcp140.dll"); return 1; }
#define LOAD(type, var, name) type var = (type)GetProcAddress(dll, name); if (!var) { puts("missing " name); return 1; }
    LOAD(map_fn, upper, "?toupper@?$ctype@D@std@@QEBADD@Z");
    LOAD(map_range_fn, upper_range, "?toupper@?$ctype@D@std@@QEBAPEBDPEADPEBD@Z");
    LOAD(map_fn, widen, "?widen@?$ctype@D@std@@QEBADD@Z");
    LOAD(copy_fn, widen_range, "?widen@?$ctype@D@std@@QEBAPEBDPEBD0PEAD@Z");
    LOAD(narrow_fn, narrow, "?narrow@?$ctype@D@std@@QEBADDD@Z");
    LOAD(narrow_range_fn, narrow_range, "?narrow@?$ctype@D@std@@QEBAPEBDPEBD0DPEAD@Z");
    LOAD(getcat_fn, getcat, "?_Getcat@?$ctype@D@std@@SA_KPEAPEBVfacet@locale@2@PEBV42@@Z");
    LOAD(classic_fn, classic, "?classic@locale@std@@SAAEBV12@XZ");
    /* _Getcat reads the locale's name (Windows dereferences it: a NULL locale crashes).
       The "C" locale, not the user's: its toupper changes only a-z. */
    const void *c_locale = classic();
    CHECK(c_locale != NULL);
    void *facet = NULL;
    CHECK(getcat(&facet, c_locale) == 2 && facet != NULL);
    if (!facet) return 1;
    char src[258], out[258], mapped[258];
    memset(src, 0x65, sizeof src);
    for (int i = 0; i < 256; ++i) {
        src[i + 1] = (char)i;
        int up = i >= 'a' && i <= 'z' ? i - 'a' + 'A' : i;
        CHECK((unsigned char)upper(facet, (char)i) == up);
        CHECK((unsigned char)widen(facet, (char)i) == i);
        CHECK((unsigned char)narrow(facet, (char)i, '?') == i);
    }
    memcpy(mapped, src, sizeof src);
    CHECK(upper_range(facet, mapped + 1, mapped + 257) == mapped + 257);
    for (int i = 0; i < 256; ++i) CHECK((unsigned char)mapped[i + 1] == (i >= 'a' && i <= 'z' ? i - 'a' + 'A' : i));
    CHECK(mapped[0] == 0x65 && mapped[257] == 0x65);
    memset(out, 0x65, sizeof out);
    CHECK(widen_range(facet, src + 1, src + 257, out + 1) == src + 257);
    CHECK(memcmp(src, out, sizeof src) == 0);
    memset(out, 0x65, sizeof out);
    CHECK(narrow_range(facet, src + 1, src + 257, '?', out + 1) == src + 257);
    CHECK(memcmp(src, out, sizeof src) == 0);
    memset(out, 0x65, sizeof out);
    CHECK(upper_range(facet, out + 1, out + 1) == out + 1);
    CHECK(widen_range(facet, src + 1, src + 1, out + 1) == src + 1);
    CHECK(narrow_range(facet, src + 1, src + 1, '?', out + 1) == src + 1);
    for (int i = 0; i < 258; ++i) CHECK(out[i] == 0x65);
    /* A user facet overrides every operation: verify virtual dispatch, all
       arguments (including narrow's fifth stack argument), and return values. */
    void *vt[11] = {0};
    vt[5] = custom_upper_range; vt[6] = custom_upper;
    vt[7] = custom_widen_range; vt[8] = custom_widen;
    vt[9] = custom_narrow_range; vt[10] = custom_narrow;
    struct { void **vtable; size_t refs; } custom = { vt, 0 };
    expected_this = &custom; expected_first = src + 1; expected_last = src + 257; expected_dest = out + 1;
    CHECK((unsigned char)upper(&custom, (char)0xe3) == 0x91);
    CHECK(upper_range(&custom, expected_first, expected_last) == expected_first + 1);
    CHECK((unsigned char)widen(&custom, (char)0xe3) == 0x92);
    CHECK(widen_range(&custom, expected_first, expected_last, expected_dest) == expected_first + 2);
    CHECK((unsigned char)narrow(&custom, (char)0xe3, (char)0x87) == 0x87);
    CHECK(narrow_range(&custom, expected_first, expected_last, (char)0x87, expected_dest) == expected_first + 3);
    CHECK(called == 63);
    typedef void *(*delete_fn)(void *, unsigned);
    ((delete_fn)(*(void ***)facet)[0])(facet, 1);
    printf("ctype<char>: %d checks, %d failures\n", checks, failures);
    return failures != 0;
}
