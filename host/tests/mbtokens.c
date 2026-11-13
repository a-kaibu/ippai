#include <windows.h>
#include <stdio.h>
#include <string.h>
static int errors;
/* (ucrtbase's invalid-parameter handler returns, so its checks answer EINVAL instead of
   ending the process; stdout unbuffered: Windows ended this probe with nothing printed) */
static void __cdecl ignore_invalid(const wchar_t *e, const wchar_t *f, const wchar_t *file, unsigned line, UINT_PTR r) {
    (void)e; (void)f; (void)file; (void)line; (void)r;
}
typedef void (__cdecl *invalid_handler)(const wchar_t *, const wchar_t *, const wchar_t *, unsigned, UINT_PTR);
static void returning_invalid_parameters(HMODULE ucrt) {
    invalid_handler (__cdecl *set)(invalid_handler) = (void *)GetProcAddress(ucrt, "_set_invalid_parameter_handler");
    if (set) set(ignore_invalid);
    setvbuf(stdout, NULL, _IONBF, 0);
}
#define CHECK(x) do { if (!(x)) { printf("FAIL line %d\n", __LINE__); errors++; } } while (0)
int main(void) {
    HMODULE m = LoadLibraryA("ucrtbase.dll");
    if (!m) return 1;
    returning_invalid_parameters(m);
    unsigned char *(__cdecl *find)(const unsigned char *, const unsigned char *) = (void *)GetProcAddress(m, "_mbsstr");
    unsigned char *(__cdecl *token)(unsigned char *, const unsigned char *, unsigned char **) = (void *)GetProcAddress(m, "_mbstok_s");
    if (!find || !token) return 1;
    unsigned char text[] = {0x81, 0x5c, 'x', '\\', 'y', 0};
    CHECK(find(text, (unsigned char *)"\\") == text + 3);
    CHECK(find(text, (unsigned char *)"") == text);
    unsigned char a[] = ",,one,two;three", b[] = "a:b", *ac = NULL, *bc = NULL;
    CHECK(!strcmp((char *)token(a, (unsigned char *)",", &ac), "one"));
    CHECK(!strcmp((char *)token(b, (unsigned char *)":", &bc), "a"));
    CHECK(!strcmp((char *)token(NULL, (unsigned char *)",;", &ac), "two"));
    CHECK(!strcmp((char *)token(NULL, (unsigned char *)":", &bc), "b"));
    CHECK(!strcmp((char *)token(NULL, (unsigned char *)",;", &ac), "three"));
    CHECK(token(NULL, (unsigned char *)",", &ac) == NULL);
    unsigned char *context = NULL;
    CHECK(token(text, (unsigned char *)"\\", &context) == text && context == text + 4);
    CHECK(text[0] == 0x81 && text[1] == 0x5c && text[3] == 0);
    CHECK(!strcmp((char *)token(NULL, (unsigned char *)"\\", &context), "y"));
    unsigned char multi[] = {'a', 0x82, 0xa0, 'b', 0}, delimiter[] = {0x82, 0xa0, 0};
    CHECK(token(multi, delimiter, &context) == multi && context == multi + 3 && multi[1] == 0);
    CHECK(!strcmp((char *)token(NULL, delimiter, &context), "b"));
    CHECK(token(NULL, NULL, &context) == NULL);
    printf("multibyte tokens errors: %d\n", errors);
    return errors != 0;
}
