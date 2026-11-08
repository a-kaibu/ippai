/* icu.dll's break iterators (#51), as AdvHD (x64) finds where its lines may wrap: ubrk_open,
 * utext_openUChars + ubrk_setUText, ubrk_current / ubrk_next to the end, ubrk_close, and
 * uloc_getDefault. The boundaries of Japanese (small kana, punctuation, brackets), English,
 * mixed, emoji and surrogate text for each kind (character, word, line, sentence); the
 * iterator's moves (previous, first, last, following, preceding, isBoundary); a UText on the
 * caller's stack (UTEXT_INITIALIZER) and one icu.dll allocates; failures in, failures out.
 * Windows 10 1903 and later carry icu.dll; the exports are cdecl and unversioned. */
#include "../probe.h"

typedef unsigned short UChar;
typedef int UErrorCode;
/* (utext.h: UText is public; sizeOfStruct is its size) */
typedef struct {
    unsigned magic;
    int flags, providerProperties, sizeOfStruct;
    long long chunkNativeLimit;
    int extraSize, nativeIndexingLimit;
    long long chunkNativeStart;
    int chunkOffset, chunkLength;
    const UChar *chunkContents;
    const void *pFuncs;
    void *pExtra;
    const void *context, *p, *q, *r;
    void *privP;
    long long a, b;
    int c;
    long long privA, privB;
    int privC;
} UText;

typedef void *(__cdecl *open_fn)(int, const char *, const UChar *, int, UErrorCode *);
typedef void(__cdecl *set_utext_fn)(void *, UText *, UErrorCode *);
typedef void(__cdecl *set_text_fn)(void *, const UChar *, int, UErrorCode *);
typedef int(__cdecl *step_fn)(void *);
typedef int(__cdecl *at_fn)(void *, int);
typedef void(__cdecl *close_fn)(void *);
typedef UText *(__cdecl *utext_open_fn)(UText *, const UChar *, long long, UErrorCode *);
typedef UText *(__cdecl *utext_close_fn)(UText *);
typedef const char *(__cdecl *loc_fn)(void);

static open_fn ubrk_open;
static set_utext_fn ubrk_setUText;
static set_text_fn ubrk_setText;
static step_fn ubrk_current, ubrk_next, ubrk_previous, ubrk_first, ubrk_last;
static at_fn ubrk_following, ubrk_preceding, ubrk_isBoundary;
static close_fn ubrk_close;
static utext_open_fn utext_openUChars;
static utext_close_fn utext_close;
static loc_fn uloc_getDefault;

static const char *kinds[] = { "character", "word", "line", "sentence" };

/* (the boundaries from current() on, by next()) */
static void walk(void *b) {
    printf(" %d", ubrk_current(b));
    for (int n, i = 0; (n = ubrk_next(b)) != -1 && i < 200; i++) printf(" %d", n);
    printf("\n");
}

static void text(const char *label, const wchar_t *w) {
    int len = (int)wcslen(w);
    for (int k = 0; k < 4; k++) {
        UErrorCode st = 0;
        void *b = ubrk_open(k, "ja_JP", (const UChar *)w, len, &st);
        printf("%s %s: %d", label, kinds[k], st);
        if (b) walk(b), ubrk_close(b);
        else printf(" no iterator\n");
    }
}

int main(void) {
    HMODULE m = LoadLibraryA("icu.dll");
    if (!m) {
        printf("skip no icu.dll\n");
        return 0;
    }
#define GET(f) f = (void *)GetProcAddress(m, #f)
    GET(ubrk_open), GET(ubrk_setUText), GET(ubrk_setText), GET(ubrk_current), GET(ubrk_next), GET(ubrk_previous);
    GET(ubrk_first), GET(ubrk_last), GET(ubrk_following), GET(ubrk_preceding), GET(ubrk_isBoundary), GET(ubrk_close);
    GET(utext_openUChars), GET(utext_close), GET(uloc_getDefault);
    printf("exports %d\n", ubrk_open && ubrk_setUText && ubrk_setText && ubrk_current && ubrk_next && ubrk_previous && ubrk_first && ubrk_last &&
                               ubrk_following && ubrk_preceding && ubrk_isBoundary && ubrk_close && utext_openUChars && utext_close && uloc_getDefault);
    if (!ubrk_open || !utext_openUChars) return 0;
    printf("uloc_getDefault %s\n", uloc_getDefault());

    text("ja", L"\x300c\x3061\x3087\x3063\x3068\x3001\x5f85\x3063\x3066\x3002\x300d\x3042\x3041\x30fc\x30c3\x30c8\x30fb\x6f22\x5b57\xff01\xff1f");
    text("ja long", L"\x543e\x8f29\x306f\x732b\x3067\x3042\x308b\x3002\x540d\x524d\x306f\x307e\x3060\x7121\x3044\x3002\x3069\x3053\x3067\x751f\x308c\x305f\x304b\x3068\x3093\x3068\x898b\x5f53\x304c\x3064\x304b\x306c\x3002");
    text("en", L"The quick (\"brown\") fox can't jump 32.3 feet, right? Yes. Mr. Smith-Jones went.");
    text("mixed", L"AdvHD\x306e\x30c6\x30ad\x30b9\x30c8 test\x3002ICU 64\x3067\x6298\x308a\x8fd4\x3059\xff08\x62ec\x5f27\xff09\x2026\x2026");
    text("emoji", L"a\xd83d\xdc68\x200d\xd83d\xdc69\x200d\xd83d\xdc67" L"b\xd83c\xddef\xd83c\xddf5" L"c\xd83d\xdc4d\xd83c\xdffd e\x301");
    text("surrogates", L"\xd840\xdc0b\xd840\xdc0b\x3042 \xd842\xdfb7\x91ce\x5bb6");
    text("spaces", L"  a  \r\nb\n\nc\t d");
    text("empty", L"");

    /* the moves, over the English text's lines */
    const wchar_t *en = L"a quick fox";
    UErrorCode st = 0;
    void *b = ubrk_open(2, "", (const UChar *)en, -1, &st);
    printf("NUL-terminated: %d", st);
    walk(b);
    /* (one call a statement: the order arguments are evaluated in is not C's to say) */
    int n1 = ubrk_next(b), n2 = ubrk_current(b), n3 = ubrk_previous(b);
    printf("after the end: next %d current %d previous %d\n", n1, n2, n3);
    int s1 = ubrk_first(b), s2 = ubrk_previous(b), s3 = ubrk_current(b), s4 = ubrk_last(b);
    printf("first %d previous %d current %d last %d\n", s1, s2, s3, s4);
    int f1 = ubrk_following(b, 2), f2 = ubrk_following(b, 11), c1 = ubrk_current(b), f3 = ubrk_following(b, -5), c2 = ubrk_current(b);
    printf("following 2: %d, 11: %d current %d, -5: %d current %d\n", f1, f2, c1, f3, c2);
    int p1 = ubrk_preceding(b, 8), p2 = ubrk_preceding(b, 0), c3 = ubrk_current(b), p3 = ubrk_preceding(b, 50), c4 = ubrk_current(b);
    printf("preceding 8: %d, 0: %d current %d, 50: %d current %d\n", p1, p2, c3, p3, c4);
    int i1 = ubrk_isBoundary(b, 8), c5 = ubrk_current(b), i2 = ubrk_isBoundary(b, 5), c6 = ubrk_current(b);
    printf("isBoundary 8: %d current %d, 5: %d current %d\n", i1, c5, i2, c6);

    /* a UText on the stack, then setUText */
    const wchar_t *ja = L"\x3042\x3063\x3002\x3044";
    UText ut;
    memset(&ut, 0, sizeof ut);
    ut.magic = 0x345ad82c, ut.sizeOfStruct = sizeof ut;
    st = 0;
    UText *u = utext_openUChars(&ut, (const UChar *)ja, 4, &st);
    printf("utext_openUChars on the stack: %d same %d\n", st, u == &ut);
    ubrk_setUText(b, u, &st);
    printf("setUText: %d", st);
    walk(b);
    printf("utext_close: same %d\n", utext_close(u) == &ut);
    /* one icu.dll allocates, NUL-terminated (-1) */
    st = 0;
    u = utext_openUChars(NULL, (const UChar *)en, -1, &st);
    printf("utext_openUChars NULL: %d allocated %d\n", st, u != NULL);
    ubrk_setUText(b, u, &st);
    printf("setUText: %d", st);
    walk(b);
    printf("utext_close: %d\n", utext_close(u) == NULL);
    /* setText */
    st = 0;
    ubrk_setText(b, (const UChar *)ja, 4, &st);
    printf("setText: %d", st);
    walk(b);
    ubrk_close(b);

    /* failures: a failure in is left as it is; a kind ICU has not */
    st = 7;
    printf("open with a failure in: %d status %d\n", ubrk_open(2, "", (const UChar *)en, -1, &st) != NULL, st);
    st = 0;
    b = ubrk_open(9, "", (const UChar *)en, -1, &st);
    printf("open kind 9: %d status failed %d\n", b != NULL, st > 0);
    if (b) ubrk_close(b);
    /* no text yet: open with NULL, then the text */
    st = 0;
    b = ubrk_open(0, "", NULL, 0, &st);
    int o1 = b ? ubrk_current(b) : -9, o2 = b ? ubrk_next(b) : -9;
    printf("open without text: %d status %d current %d next %d\n", b != NULL, st, o1, o2);
    if (b) ubrk_close(b);
    st = 7;
    u = utext_openUChars(NULL, (const UChar *)en, -1, &st);
    printf("utext_openUChars with a failure in: %d status %d\n", u != NULL, st);
    return 0;
}
