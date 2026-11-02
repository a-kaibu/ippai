/* C library functions sake runs as guest x86 code instead of trapping to Rust: the ones that
 * call back into the game (qsort, bsearch: a trap per comparison was 6x slower than Wine) and
 * the small string/memory functions programs call millions of times (a trap costs more than
 * the work). Large copies and fills still go to the host (host_* slots, set by guest.rs).
 * Built by gen-guest.sh at a fixed address (crates/sake/src/guest.rs GUEST_AT); freestanding,
 * cdecl, msvcrt's results (comparisons give -1, 0 or 1). */
typedef unsigned int size_t;
typedef unsigned short wchar_t;
#define BIG 64

void *(*host_memmove)(void *, const void *, size_t);
void *(*host_memset)(void *, int, size_t);
/* malloc and free: sake's x86 stubs (small blocks from free lists), realloc: the trap */
void *(*host_malloc)(size_t);
void (*host_free)(void *);
void *(*host_realloc)(void *, size_t);
#define SLAB 0x6e697777u /* mem.rs: a small block's header word before it (its size before that) */
#define SLAB_MAX 512

void *memmove(void *d, const void *s, size_t n) {
    if (n >= BIG) return host_memmove(d, s, n);
    unsigned char *dp = d;
    const unsigned char *sp = s;
    if (dp <= sp || dp >= sp + n) {
        while (n >= 4) { *(unsigned *)dp = *(const unsigned *)sp; dp += 4; sp += 4; n -= 4; }
        while (n--) *dp++ = *sp++;
    } else {
        dp += n; sp += n;
        while (n--) *--dp = *--sp;
    }
    return d;
}
void *memcpy(void *d, const void *s, size_t n) { return memmove(d, s, n); }
void *memset(void *d, int c, size_t n) {
    if (n >= BIG) return host_memset(d, c, n);
    unsigned char *p = d;
    while (n--) *p++ = (unsigned char)c;
    return d;
}
int memcmp(const void *a, const void *b, size_t n) {
    const unsigned char *x = a, *y = b;
    for (; n; n--, x++, y++) if (*x != *y) return *x < *y ? -1 : 1;
    return 0;
}
void *memchr(const void *s, int c, size_t n) {
    const unsigned char *p = s;
    for (; n; n--, p++) if (*p == (unsigned char)c) return (void *)p;
    return 0;
}
size_t strlen(const char *s) {
    const char *p = s;
    while (*p) p++;
    return p - s;
}
int strcmp(const char *a, const char *b) {
    const unsigned char *x = (const unsigned char *)a, *y = (const unsigned char *)b;
    while (*x && *x == *y) x++, y++;
    return *x == *y ? 0 : *x < *y ? -1 : 1;
}
int strncmp(const char *a, const char *b, size_t n) {
    const unsigned char *x = (const unsigned char *)a, *y = (const unsigned char *)b;
    for (; n; n--, x++, y++) {
        if (*x != *y) return *x < *y ? -1 : 1;
        if (!*x) return 0;
    }
    return 0;
}
char *strcpy(char *d, const char *s) {
    char *r = d;
    while ((*d++ = *s++)) {}
    return r;
}
char *strncpy(char *d, const char *s, size_t n) {
    char *r = d;
    for (; n && *s; n--) *d++ = *s++;
    for (; n; n--) *d++ = 0;
    return r;
}
char *strcat(char *d, const char *s) {
    strcpy(d + strlen(d), s);
    return d;
}
char *strncat(char *d, const char *s, size_t n) {
    char *p = d + strlen(d);
    for (; n && *s; n--) *p++ = *s++;
    *p = 0;
    return d;
}
char *strchr(const char *s, int c) {
    for (;; s++) {
        if (*s == (char)c) return (char *)s;
        if (!*s) return 0;
    }
}
char *strrchr(const char *s, int c) {
    const char *r = 0;
    for (;; s++) {
        if (*s == (char)c) r = s;
        if (!*s) return (char *)r;
    }
}
char *strstr(const char *h, const char *n) {
    if (!*n) return (char *)h;
    for (; *h; h++) {
        const char *a = h, *b = n;
        while (*a && *a == *b) a++, b++;
        if (!*b) return (char *)h;
    }
    return 0;
}
static int in(const char *set, char c) {
    for (; *set; set++) if (*set == c) return 1;
    return 0;
}
size_t strspn(const char *s, const char *set) {
    const char *p = s;
    while (*p && in(set, *p)) p++;
    return p - s;
}
size_t strcspn(const char *s, const char *set) {
    const char *p = s;
    while (*p && !in(set, *p)) p++;
    return p - s;
}
char *strpbrk(const char *s, const char *set) {
    for (; *s; s++) if (in(set, *s)) return (char *)s;
    return 0;
}
size_t wcslen(const wchar_t *s) {
    const wchar_t *p = s;
    while (*p) p++;
    return p - s;
}
int wcscmp(const wchar_t *a, const wchar_t *b) {
    while (*a && *a == *b) a++, b++;
    return *a == *b ? 0 : *a < *b ? -1 : 1;
}
wchar_t *wcscpy(wchar_t *d, const wchar_t *s) {
    wchar_t *r = d;
    while ((*d++ = *s++)) {}
    return r;
}
wchar_t *wcschr(const wchar_t *s, wchar_t c) {
    for (;; s++) {
        if (*s == c) return (wchar_t *)s;
        if (!*s) return 0;
    }
}

typedef int (*cmp_fn)(const void *, const void *);
static void swap(char *a, char *b, size_t w) {
    if (a == b) return;
    if (!(w & 3)) {
        for (; w; w -= 4, a += 4, b += 4) { unsigned t = *(unsigned *)a; *(unsigned *)a = *(unsigned *)b; *(unsigned *)b = t; }
        return;
    }
    for (; w; w--, a++, b++) { char t = *a; *a = *b; *b = t; }
}
/* quicksort (median of three, the smaller side first so the stack stays small), insertion sort
 * below 8 elements */
static void sort(char *base, size_t n, size_t w, cmp_fn f) {
    while (n > 7) {
        char *lo = base, *mid = base + (n / 2) * w, *hi = base + (n - 1) * w;
        if (f(mid, lo) < 0) swap(mid, lo, w);
        if (f(hi, mid) < 0) {
            swap(hi, mid, w);
            if (f(mid, lo) < 0) swap(mid, lo, w);
        }
        swap(mid, lo, w); /* pivot at base */
        char *i = base + w, *j = hi;
        for (;;) {
            while (i <= j && f(i, base) < 0) i += w;
            while (i <= j && f(base, j) < 0) j -= w;
            if (i >= j) break;
            swap(i, j, w);
            i += w;
            j -= w;
        }
        swap(base, j, w);
        size_t left = (j - base) / w, right = n - left - 1;
        if (left < right) {
            sort(base, left, w, f);
            base = j + w;
            n = right;
        } else {
            sort(j + w, right, w, f);
            n = left;
        }
    }
    for (size_t k = 1; k < n; k++)
        for (char *p = base + k * w; p > base && f(p - w, p) > 0; p -= w) swap(p - w, p, w);
}
void qsort(void *base, size_t n, size_t w, cmp_fn f) {
    if (n > 1 && w) sort(base, n, w, f);
}
void *bsearch(const void *key, const void *base, size_t n, size_t w, cmp_fn f) {
    size_t lo = 0, hi = n;
    while (lo < hi) {
        size_t mid = (lo + hi) / 2;
        const char *p = (const char *)base + mid * w;
        int r = f(key, p);
        if (!r) return (void *)p;
        if (r < 0) hi = mid; else lo = mid + 1;
    }
    return 0;
}

/* realloc of a small block (Lua reallocates its objects all the time): within its 16-byte
 * class in place, else a new small block; anything else is the host's */
void *realloc(void *p, size_t n) {
    if (!p) return host_malloc(n);
    unsigned *h = (unsigned *)p;
    if (h[-1] != SLAB || !n) return host_realloc(p, n);
    unsigned old = h[-2], cap = (old + 15) >> 4;
    if (!cap) cap = 1;
    if (n <= SLAB_MAX && ((n + 15) >> 4) <= cap) {
        h[-2] = n;
        return p;
    }
    void *q = host_malloc(n);
    if (!q) return 0;
    memmove(q, p, old < n ? old : n);
    host_free(p);
    return q;
}

/* <ctype.h> in the "C" locale (sake's only one): ASCII, nothing above 0x7f */
static int in_range(int c) { return c >= 0 && c < 128; }
int isupper(int c) { return c >= 'A' && c <= 'Z'; }
int islower(int c) { return c >= 'a' && c <= 'z'; }
int isdigit(int c) { return c >= '0' && c <= '9'; }
int isalpha(int c) { return isupper(c) || islower(c); }
int isalnum(int c) { return isalpha(c) || isdigit(c); }
int isxdigit(int c) { return isdigit(c) || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F'); }
int isspace(int c) { return c == ' ' || (c >= 9 && c <= 13); }
int iscntrl(int c) { return in_range(c) && (c < 32 || c == 127); }
int isprint(int c) { return c >= 32 && c < 127; }
int isgraph(int c) { return c > 32 && c < 127; }
int ispunct(int c) { return isgraph(c) && !isalnum(c); }
int tolower(int c) { return isupper(c) ? c + 32 : c; }
int toupper(int c) { return islower(c) ? c - 32 : c; }
