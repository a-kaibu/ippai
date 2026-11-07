/* sake's fast paths of hot imports (fast.rs x86 stubs, guest/crt.c's x64 forms and C library):
 * critical sections (recursion, contention across threads), last error, TLS, HeapAlloc /
 * HeapFree / HeapReAlloc of small and big blocks (HEAP_ZERO_MEMORY), malloc / realloc / free,
 * _errno, qsort and bsearch (msvcrt's order of equal keys), the string functions. Built for x86
 * and x64: both must print the same, with the JIT and without (nothing printed depends on sake's
 * own layouts). */
#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>

static CRITICAL_SECTION cs;
static volatile LONG counter;
static unsigned hash = 2166136261u;
static void mix(unsigned v) { hash = (hash ^ v) * 16777619u; }

static DWORD WINAPI worker(void *arg)
{
    for (int i = 0; i < 20000; i++)
    {
        EnterCriticalSection(&cs);
        EnterCriticalSection(&cs); /* recursion */
        counter++;
        LeaveCriticalSection(&cs);
        LeaveCriticalSection(&cs);
    }
    return (DWORD)(size_t)arg;
}

struct item { int key, order; };
static int by_key(const void *a, const void *b) { return ((const struct item *)a)->key - ((const struct item *)b)->key; }

int main(void)
{
    /* critical sections */
    InitializeCriticalSection(&cs);
    HANDLE th[4];
    for (int i = 0; i < 4; i++) th[i] = CreateThread(NULL, 0, worker, (void *)(size_t)(i + 1), 0, NULL);
    WaitForMultipleObjects(4, th, TRUE, INFINITE);
    DWORD codes = 0;
    for (int i = 0; i < 4; i++) { DWORD c; GetExitCodeThread(th[i], &c); codes = codes * 10 + c; CloseHandle(th[i]); }
    printf("cs: %ld (exit codes %lu), try %d\n", counter, codes, TryEnterCriticalSection(&cs));
    LeaveCriticalSection(&cs);
    DeleteCriticalSection(&cs);

    /* last error, TLS */
    SetLastError(1234);
    DWORD e1 = GetLastError();
    DWORD slot = TlsAlloc();
    TlsSetValue(slot, (void *)0x5678);
    SetLastError(99);
    void *v = TlsGetValue(slot);
    void *u = TlsGetValue(TlsAlloc());
    printf("last error %lu, tls %lx error %lu, unset %lx\n", e1, (unsigned long)(size_t)v, GetLastError(), (unsigned long)(size_t)u);

    /* heap: small blocks come back zeroed when asked, sizes kept, reuse */
    HANDLE heap = GetProcessHeap();
    unsigned char *blocks[64];
    for (int round = 0; round < 3; round++)
    {
        for (int i = 0; i < 64; i++)
        {
            SIZE_T n = (SIZE_T)(i * 37 % 700) + 1;
            blocks[i] = HeapAlloc(heap, HEAP_ZERO_MEMORY, n);
            int zero = 1;
            for (SIZE_T k = 0; k < n; k++) zero &= blocks[i][k] == 0;
            mix(zero + (unsigned)HeapSize(heap, 0, blocks[i]) * 3 + (((size_t)blocks[i] & 7) == 0) * 7);
            memset(blocks[i], 0xab, n);
        }
        for (int i = 0; i < 64; i += 2)
        {
            SIZE_T n = (SIZE_T)(i * 37 % 700) + 1;
            blocks[i] = HeapReAlloc(heap, 0, blocks[i], 40);
            mix(blocks[i][(n < 40 ? n : 40) - 1] + blocks[i][0] + (unsigned)HeapSize(heap, 0, blocks[i]));
        }
        for (int i = 0; i < 64; i++) mix(HeapFree(heap, 0, blocks[i]));
    }
    mix(HeapFree(heap, 0, NULL));
    printf("heap %08x\n", hash);

    /* malloc / realloc / free */
    hash = 2166136261u;
    char *p[100];
    for (int i = 0; i < 100; i++) { p[i] = malloc(i * 11 + 1); memset(p[i], i, i * 11 + 1); }
    for (int i = 0; i < 100; i++) { p[i] = realloc(p[i], i * 13 + 2); mix((unsigned char)p[i][i * 11] + (((size_t)p[i] & 7) == 0)); }
    for (int i = 0; i < 100; i++) free(p[i]);
    free(NULL);
    errno = 0;
    strtol("99999999999999999999", NULL, 10);
    printf("malloc %08x, errno %d\n", hash, errno);

    /* qsort / bsearch: equal keys in msvcrt's order */
    struct item it[300];
    unsigned r = 1;
    for (int i = 0; i < 300; i++) { r = r * 1103515245 + 12345; it[i].key = (r >> 16) % 40; it[i].order = i; }
    qsort(it, 300, sizeof(it[0]), by_key);
    hash = 2166136261u;
    for (int i = 0; i < 300; i++) mix(it[i].key * 1000 + it[i].order);
    struct item k = { 17, 0 };
    struct item *f = bsearch(&k, it, 300, sizeof(it[0]), by_key);
    printf("qsort %08x, bsearch %d\n", hash, f ? (int)(f - it) : -1);

    /* strings */
    char s[64];
    strcpy(s, "hello, ");
    strcat(s, "world");
    wchar_t w[16] = L"abc";
    printf("str %d %d %d %s %d %d %d\n", (int)strlen(s), strcmp(s, "hello"), strncmp(s, "help", 3), strchr(s, 'w'), (int)strspn(s, "hel"), (int)wcslen(w), memcmp(s, "hellp", 5));
    return 0;
}
