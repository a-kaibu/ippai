/* The environment the other probes see: code pages and locales (Windows' are whatever the runner
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
    /* the same answer through two ways, whatever the locale: "Japan only" checks compare them */
    HKL hkl = 0;
    BOOL r = SystemParametersInfoA(SPI_GETDEFAULTINPUTLANG, 0, &hkl, 0);
    printf("SPI_GETDEFAULTINPUTLANG ret %d same as GetKeyboardLayout(0) %d\n", r, hkl == GetKeyboardLayout(0));
    TIME_ZONE_INFORMATION z;
    GetTimeZoneInformation(&z);
    _tzset();
    printf("_timezone is Bias * 60 %d\n", _timezone == z.Bias * 60L);
    return 0;
}
