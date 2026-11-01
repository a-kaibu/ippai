/*
 * C library functions a wasm DLL keeps to itself instead of calling the x86 ones through its
 * imports (pure functions: memory and strings). Linked as a library: only what the DLL uses.
 * clang also makes calls to some of these on its own (strcpy for sprintf("%s"), ...).
 * Compiled with -fno-builtin; bulk memory makes __builtin_memcpy etc. single instructions.
 */
typedef __SIZE_TYPE__ size_t;
typedef unsigned short wchar;

void *memcpy( void *d, const void *s, size_t n ) { __builtin_memcpy( d, s, n ); return d; }
void *memmove( void *d, const void *s, size_t n ) { __builtin_memmove( d, s, n ); return d; }
void *memset( void *d, int c, size_t n ) { __builtin_memset( d, c, n ); return d; }

int memcmp( const void *a, const void *b, size_t n )
{
    const unsigned char *p = a, *q = b;
    for (; n; n--, p++, q++) if (*p != *q) return *p - *q;
    return 0;
}

void *memchr( const void *s, int c, size_t n )
{
    const unsigned char *p = s;
    for (; n; n--, p++) if (*p == (unsigned char)c) return (void *)p;
    return 0;
}

size_t strlen( const char *s ) { const char *p = s; while (*p) p++; return p - s; }
size_t strnlen( const char *s, size_t n ) { size_t i = 0; while (i < n && s[i]) i++; return i; }
char *strcpy( char *d, const char *s ) { char *r = d; while ((*d++ = *s++)); return r; }
char *stpcpy( char *d, const char *s ) { while ((*d = *s++)) d++; return d; }
char *strcat( char *d, const char *s ) { strcpy( d + strlen( d ), s ); return d; }
char *strncpy( char *d, const char *s, size_t n )
{
    size_t i = 0;
    for (; i < n && s[i]; i++) d[i] = s[i];
    for (; i < n; i++) d[i] = 0;
    return d;
}
int strcmp( const char *a, const char *b )
{
    while (*a && *a == *b) a++, b++;
    return (unsigned char)*a - (unsigned char)*b;
}
int strncmp( const char *a, const char *b, size_t n )
{
    for (; n; n--, a++, b++)
    {
        if (*a != *b) return (unsigned char)*a - (unsigned char)*b;
        if (!*a) return 0;
    }
    return 0;
}
char *strchr( const char *s, int c )
{
    for (;; s++)
    {
        if (*s == (char)c) return (char *)s;
        if (!*s) return 0;
    }
}
char *strrchr( const char *s, int c )
{
    const char *r = 0;
    for (;; s++)
    {
        if (*s == (char)c) r = s;
        if (!*s) return (char *)r;
    }
}

size_t wcslen( const wchar *s ) { const wchar *p = s; while (*p) p++; return p - s; }
size_t wcsnlen( const wchar *s, size_t n ) { size_t i = 0; while (i < n && s[i]) i++; return i; }
wchar *wcscpy( wchar *d, const wchar *s ) { wchar *r = d; while ((*d++ = *s++)); return r; }
wchar *wcscat( wchar *d, const wchar *s ) { wcscpy( d + wcslen( d ), s ); return d; }
int wcscmp( const wchar *a, const wchar *b )
{
    while (*a && *a == *b) a++, b++;
    return (*a > *b) - (*a < *b);
}
int wcsncmp( const wchar *a, const wchar *b, size_t n )
{
    for (; n; n--, a++, b++)
    {
        if (*a != *b) return (*a > *b) - (*a < *b);
        if (!*a) return 0;
    }
    return 0;
}
wchar *wcschr( const wchar *s, wchar c )
{
    for (;; s++)
    {
        if (*s == c) return (wchar *)s;
        if (!*s) return 0;
    }
}
wchar *wcsrchr( const wchar *s, wchar c )
{
    const wchar *r = 0;
    for (;; s++)
    {
        if (*s == c) r = s;
        if (!*s) return (wchar *)r;
    }
}
