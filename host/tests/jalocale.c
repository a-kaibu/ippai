/* libs: -luser32 */
/* The "Japan only" checks games make before they start, as a Japanese Windows answers them
 * (sake plays one). Triangle's 妄想ぷろとこる！ (DLPC03.EXE, 2010) ANDs every locale id it can
 * get with 0x411, wants code page 932 twice and the C runtime's _timezone at -9 h, and quits
 * with "本製品は日本国内専用です。" otherwise; SPI_GETDEFAULTINPUTLANG answered 0 on sake. */
#include <windows.h>
#include <stdio.h>
#include <time.h>
#include <string.h>
static int failed;
#define CHECK(label, expr) do { int ok = !!(expr); printf("%s: %s\n", label, ok ? "ok" : "FAIL"); failed |= !ok; } while (0)
#define JA(x) (((x) & 0x411) == 0x411)
int main(void) {
    HKL spi = 0, list[4] = {0};
    BOOL r = SystemParametersInfoA(SPI_GETDEFAULTINPUTLANG, 0, &spi, 0);
    CHECK("SPI_GETDEFAULTINPUTLANG returns TRUE", r);
    CHECK("SPI_GETDEFAULTINPUTLANG is Japanese", JA((UINT_PTR)spi));
    CHECK("SPI_GETDEFAULTINPUTLANG == GetKeyboardLayout(0)", spi == GetKeyboardLayout(0));
    CHECK("GetKeyboardLayoutList has it", GetKeyboardLayoutList(4, list) >= 1 && list[0] == spi);
    CHECK("UI languages Japanese", JA(GetSystemDefaultUILanguage()) && JA(GetUserDefaultUILanguage()));
    CHECK("lang ids Japanese", JA(GetSystemDefaultLangID()) && JA(GetUserDefaultLangID()));
    CHECK("LCIDs Japanese", JA(GetSystemDefaultLCID()) && JA(GetUserDefaultLCID()) && JA(GetThreadLocale()));
    CHECK("code pages 932", GetACP() == 932 && GetOEMCP() == 932);
    CHECK("keyboard type Japanese", GetKeyboardType(0) == 7 && GetKeyboardType(1) == 2);
    _tzset();
    CHECK("_timezone -9 h", _timezone == -32400);
    CHECK("_tzname Tokyo", !strcmp(_tzname[0], "\x93\x8c\x8b\x9e (\x95\x57\x8f\x80\x8e\x9e)"));
    time_t t = 400 * 86400 + 3600;
    int lh = localtime(&t)->tm_hour, gh = gmtime(&t)->tm_hour;
    CHECK("localtime = gmtime - _timezone", (lh - gh + 24) % 24 == (-_timezone / 3600 + 24) % 24);
    return failed;
}
