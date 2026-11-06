/* manifest: ja
 * The environment the other probes see: code pages and locales (Windows' are whatever the runner
 * has; the "-ja" build asks for code page 932 through its manifest), OS version. Informational:
 * known.txt accepts its differences. */
#include "../probe.h"

int main(void) {
    printf("GetACP %u GetOEMCP %u\n", GetACP(), GetOEMCP());
    printf("GetUserDefaultLCID %lx GetSystemDefaultLCID %lx GetThreadLocale %lx\n", GetUserDefaultLCID(),
           GetSystemDefaultLCID(), GetThreadLocale());
    printf("GetUserDefaultUILanguage %x\n", GetUserDefaultUILanguage());
    printf("GetConsoleCP %u\n", GetConsoleCP());
    OSVERSIONINFOA v = { sizeof v };
    GetVersionExA(&v);
    printf("GetVersionEx %lu.%lu.%lu %lu\n", v.dwMajorVersion, v.dwMinorVersion, v.dwBuildNumber, v.dwPlatformId);
    printf("lstrcmpiA ja %d\n", lstrcmpiA("\x82\xa0", "\x82\xa1"));
    return 0;
}
