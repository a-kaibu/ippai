/* C library functions sake runs as guest x86 code instead of trapping to Rust: the ones that
 * call back into the game (qsort, bsearch: a trap per comparison was 6x slower than Wine) and
 * the small string/memory functions programs call millions of times (a trap costs more than
 * the work). Large copies and fills still go to the host (host_* slots, set by guest.rs).
 * Built by gen-guest.sh at a fixed address (crates/sake/src/guest.rs GUEST_AT); freestanding,
 * cdecl, msvcrt's results (comparisons give -1, 0 or 1). Built for x64 too, with the x64 forms
 * of fast.rs's x86 stubs at the end (critical sections, last error, TLS, small heap blocks). */
#ifdef __x86_64__
/* (the x64 build, gen-guest.sh: Microsoft's calling convention, LLP64) */
typedef unsigned long long size_t;
#else
typedef unsigned int size_t;
#endif
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
/* msvcrt's own quicksort, step for step (median of three; the largest moved to the end below
 * 9 elements; an explicit stack), so that equal keys end up in the order Windows leaves them
 * (tests/winref crt_misc) */
static void shortsort(char *lo, char *hi, size_t w, cmp_fn f) {
    while (hi > lo) {
        char *max = lo;
        for (char *p = lo + w; p <= hi; p += w)
            if (f(p, max) > 0) max = p;
        swap(max, hi, w);
        hi -= w;
    }
}
void qsort(void *base, size_t num, size_t w, cmp_fn f) {
    char *lostk[32], *histk[32];
    int sp = 0;
    if (num < 2 || !w) return;
    char *lo = base, *hi = (char *)base + w * (num - 1);
    for (;;) {
        size_t size = (hi - lo) / w + 1;
        if (size <= 8) {
            shortsort(lo, hi, w, f);
        } else {
            char *mid = lo + (size / 2) * w;
            if (f(lo, mid) > 0) swap(lo, mid, w);
            if (f(lo, hi) > 0) swap(lo, hi, w);
            if (f(mid, hi) > 0) swap(mid, hi, w);
            char *loguy = lo, *higuy = hi;
            for (;;) {
                if (mid > loguy) do loguy += w; while (loguy < mid && f(loguy, mid) <= 0);
                if (mid <= loguy) do loguy += w; while (loguy <= hi && f(loguy, mid) <= 0);
                do higuy -= w; while (higuy > mid && f(higuy, mid) > 0);
                if (higuy < loguy) break;
                swap(loguy, higuy, w);
                if (mid == higuy) mid = loguy;
            }
            higuy += w;
            if (mid < higuy) do higuy -= w; while (higuy > mid && f(higuy, mid) == 0);
            if (mid >= higuy) do higuy -= w; while (higuy > lo && f(higuy, mid) == 0);
            if (higuy - lo >= hi - loguy) {
                if (lo < higuy) { lostk[sp] = lo; histk[sp] = higuy; ++sp; }
                if (loguy < hi) { lo = loguy; continue; }
            } else {
                if (loguy < hi) { lostk[sp] = loguy; histk[sp] = hi; ++sp; }
                if (lo < higuy) { hi = higuy; continue; }
            }
        }
        if (--sp < 0) return;
        lo = lostk[sp];
        hi = histk[sp];
    }
}
/* msvcrt's bsearch: which of equal elements it returns follows its halving */
void *bsearch(const void *key, const void *base, size_t num, size_t w, cmp_fn f) {
    const char *lo = base, *hi = (const char *)base + (num - 1) * w;
    if (!num) return 0;
    while (lo <= hi) {
        size_t half = num / 2;
        if (half) {
            const char *mid = lo + (num & 1 ? half : half - 1) * w;
            int r = f(key, mid);
            if (!r) return (void *)mid;
            if (r < 0) { hi = mid - w; num = num & 1 ? half : half - 1; }
            else { lo = mid + w; num = half; }
        } else if (num) {
            return f(key, lo) ? 0 : (void *)lo;
        } else {
            break;
        }
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

#ifdef __x86_64__
/* fast.rs's x86 stubs for x64: the common case here, the trap (host_*) for the rest. They keep
 * k32's layouts: a critical section's OwningThread (+12) is its lock word, +16 counts waiters;
 * the thread id at TEB+0x48, the last error at TEB+0x68, TLS slots at TEB+0x1480 (8 bytes
 * each, read as 32 bits like the trap), errno at TEB+0x1000 (crt.rs ERRNO_TEB). */
typedef unsigned int u32;
#define TEB32(o) (*(volatile u32 __seg_gs *)(size_t)(o))
#define TEB64(o) (*(volatile unsigned long long __seg_gs *)(size_t)(o))
void (*host_enter_cs)(volatile u32 *);
void (*host_cs_wake)(volatile u32 *);
void *(*host_tls_get)(u32);
void *(*host_heap_alloc)(void *, u32, size_t);
int (*host_heap_free)(void *, u32, void *);
/* mem.rs slab_table(): a spin lock word, then the free list of class k (k*16 bytes) at +4k */
volatile u32 *slab_table;

void EnterCriticalSection(volatile u32 *cs) {
    u32 me = TEB32(0x48), z = 0;
    if (cs[3] == me) { cs[2]++; return; }
    if (__atomic_compare_exchange_n(&cs[3], &z, me, 0, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) {
        cs[2] = 1;
        cs[1] = 0;
        return;
    }
    host_enter_cs(cs);
}
void LeaveCriticalSection(volatile u32 *cs) {
    int n = (int)cs[2];
    if (n > 1) { cs[2] = n - 1; return; }
    cs[2] = 0;
    cs[1] = 0xffffffff;
    __atomic_exchange_n(&cs[3], 0, __ATOMIC_SEQ_CST);
    if (cs[4]) host_cs_wake(cs);
}
u32 GetLastError(void) { return TEB32(0x68); }
void SetLastError(u32 e) { TEB32(0x68) = e; }
void *TlsGetValue(u32 i) {
    if (i >= 64) return host_tls_get(i);
    TEB32(0x68) = 0;
    return (void *)(size_t)TEB32(0x1480 + 8 * i);
}
int *_errno(void) { return (int *)(TEB64(0x30) + 0x1000); }

static void slab_lock(void) {
    while (__atomic_exchange_n(&slab_table[0], 1, __ATOMIC_ACQUIRE)) __builtin_ia32_pause();
}
static void slab_unlock(void) { __atomic_store_n(&slab_table[0], 0, __ATOMIC_RELEASE); }
/* a block of `n` (<= SLAB_MAX) bytes from its class list, or 0 (empty: the host refills it) */
static void *slab_pop(size_t n) {
    u32 k = (u32)(n + 15) >> 4;
    if (!k) k = 1;
    slab_lock();
    u32 p = slab_table[k];
    if (p) slab_table[k] = *(volatile u32 *)(size_t)p;
    slab_unlock();
    if (!p) return 0;
    u32 *h = (u32 *)(size_t)p;
    h[-2] = (u32)n;
    h[-1] = SLAB;
    return h;
}
static int slab_push(void *p) {
    u32 *h = p;
    if (h[-1] != SLAB) return 0;
    u32 k = (h[-2] + 15) >> 4;
    if (!k) k = 1;
    slab_lock();
    h[-1] = SLAB + 1; /* SLAB_FREE */
    h[0] = slab_table[k];
    slab_table[k] = (u32)(size_t)p;
    slab_unlock();
    return 1;
}
void *HeapAlloc(void *heap, u32 flags, size_t n) {
    if (n <= SLAB_MAX) {
        unsigned char *p = slab_pop(n);
        if (p) {
            if (flags & 8)
                for (size_t i = 0; i < n; i++) p[i] = 0;
            return p;
        }
    }
    return host_heap_alloc(heap, flags, n);
}
int HeapFree(void *heap, u32 flags, void *p) {
    if (!p || slab_push(p)) return 1;
    return host_heap_free(heap, flags, p);
}
void *malloc(size_t n) {
    void *p = n <= SLAB_MAX ? slab_pop(n) : 0;
    return p ? p : host_malloc(n);
}
void free(void *p) {
    if (p && !slab_push(p)) host_free(p);
}
#endif
