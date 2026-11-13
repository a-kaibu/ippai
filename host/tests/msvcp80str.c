/* MSVCP80.dll's std::string, as KiriKiri's csvParser.dll (x86) uses it (#195): the default
 * constructor, operator+=(char) one character at a time past the 15-character SSO buffer,
 * c_str/size, then the destructor. Called through GetProcAddress, thiscall (this in ECX). */
#include <stdio.h>
#include <string.h>
#include <windows.h>

#define S "?$basic_string@DU?$char_traits@D@std@@V?$allocator@D@2@@std@@"
typedef void *(__attribute__((thiscall)) *ctor_t)(void *);
typedef void *(__attribute__((thiscall)) *addc_t)(void *, char);
typedef const char *(__attribute__((thiscall)) *cstr_t)(const void *);
typedef unsigned (__attribute__((thiscall)) *size_t_t)(const void *);
typedef void (__attribute__((thiscall)) *dtor_t)(void *);

int main(void)
{
    HMODULE m = LoadLibraryA("msvcp80.dll");
    printf("LoadLibrary msvcp80: %s\n", m ? "ok" : "none");
    if (!m)
        return 0;
    ctor_t ctor = (ctor_t)GetProcAddress(m, "??0" S "QAE@XZ");
    addc_t addc = (addc_t)GetProcAddress(m, "??Y" S "QAEAAV01@D@Z");
    cstr_t cstr = (cstr_t)GetProcAddress(m, "?c_str@" S "QBEPBDXZ");
    size_t_t size = (size_t_t)GetProcAddress(m, "?size@" S "QBEIXZ");
    dtor_t dtor = (dtor_t)GetProcAddress(m, "??1" S "QAE@XZ");
    printf("exports: %s\n", ctor && addc && cstr && size && dtor ? "ok" : "missing");
    if (!(ctor && addc && cstr && size && dtor))
        return 0;
    unsigned char str[64];
    memset(str, 0xcc, sizeof str);
    printf("ctor returns this: %s\n", ctor(str) == str ? "yes" : "no");
    printf("empty: size %u, c_str \"%s\"\n", size(str), cstr(str));
    char want[101];
    for (int i = 0; i < 100; i++) {
        want[i] = 'a' + i % 26;
        if (addc(str, want[i]) != str) {
            printf("operator+= returned another object at %d\n", i);
            return 0;
        }
        want[i + 1] = 0;
        if (size(str) != (unsigned)i + 1 || strcmp(cstr(str), want)) {
            printf("mismatch after %d chars: size %u\n", i + 1, size(str));
            return 0;
        }
    }
    printf("100 appends: size %u, ends \"%s\"\n", size(str), cstr(str) + 90);
    addc(str, ',');
    addc(str, '\0');
    printf("with NUL: size %u, strlen %u\n", size(str), (unsigned)strlen(cstr(str)));
    dtor(str);
    printf("dtor ok\n");
    return 0;
}
