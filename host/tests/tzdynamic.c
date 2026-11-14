/* The time zone as .NET's TimeZoneInfo.Local asks for it (Mono in Unity 6000.3: ダイスレギオン
 * 体験版 threw EntryPointNotFoundException GetDynamicTimeZoneInformation while ZipArchive dated
 * its xlsx entries): GetDynamicTimeZoneInformation agrees with GetTimeZoneInformation and names
 * the zone's key, GetTimeZoneInformationForYear gives the same zone for any year (NULL: the
 * current zone; the zone's own key too) and fails for a key that is no zone. The zone is the
 * machine's, so only agreements are printed. (sake: k32.rs) */
#define _WIN32_WINNT 0x0602
#include <windows.h>
#include <stdio.h>
#include <string.h>

typedef DWORD (WINAPI *gdtzi_t)(DYNAMIC_TIME_ZONE_INFORMATION *);
typedef BOOL (WINAPI *gtzify_t)(USHORT, DYNAMIC_TIME_ZONE_INFORMATION *, TIME_ZONE_INFORMATION *);

static int same(const TIME_ZONE_INFORMATION *a, const TIME_ZONE_INFORMATION *b)
{
    return a->Bias == b->Bias && a->StandardBias == b->StandardBias && a->DaylightBias == b->DaylightBias &&
           !wcscmp(a->StandardName, b->StandardName) && !memcmp(&a->StandardDate, &b->StandardDate, sizeof a->StandardDate) &&
           !memcmp(&a->DaylightDate, &b->DaylightDate, sizeof a->DaylightDate);
}

int main(void)
{
    HMODULE k = GetModuleHandleA("kernel32.dll");
    gdtzi_t gdtzi = (gdtzi_t)GetProcAddress(k, "GetDynamicTimeZoneInformation");
    gtzify_t gtzify = (gtzify_t)GetProcAddress(k, "GetTimeZoneInformationForYear");
    printf("GetDynamicTimeZoneInformation %d GetTimeZoneInformationForYear %d\n", gdtzi != NULL, gtzify != NULL);
    if (!gdtzi || !gtzify)
        return 1;
    TIME_ZONE_INFORMATION tzi;
    DYNAMIC_TIME_ZONE_INFORMATION d;
    memset(&tzi, 0xcc, sizeof tzi);
    DWORD r1 = GetTimeZoneInformation(&tzi);
    memset(&d, 0xcc, sizeof d);
    DWORD r2 = gdtzi(&d);
    printf("same return %d, same zone %d\n", r1 == r2, same(&tzi, (TIME_ZONE_INFORMATION *)&d));
    size_t keylen = wcsnlen(d.TimeZoneKeyName, 128);
    printf("key name set %d, ends in the field %d, dynamic daylight disabled %d\n", keylen > 0, keylen < 128, d.DynamicDaylightTimeDisabled);
    printf("sizes %d %d\n", (int)sizeof tzi, (int)sizeof d);
    int all = 1;
    for (USHORT y = 1990; y <= 2040; y += 5) {
        TIME_ZONE_INFORMATION t;
        memset(&t, 0xcc, sizeof t);
        BOOL ok = gtzify(y, NULL, &t);
        all &= ok && t.Bias == tzi.Bias && t.StandardBias == tzi.StandardBias;
    }
    printf("for year, current zone: same bias 1990..2040 %d\n", all);
    TIME_ZONE_INFORMATION t;
    BOOL ok = gtzify(2025, &d, &t);
    printf("for year, own key: %d same bias %d\n", ok, ok && t.Bias == tzi.Bias);
    DYNAMIC_TIME_ZONE_INFORMATION bad;
    memset(&bad, 0, sizeof bad);
    wcscpy(bad.TimeZoneKeyName, L"No Such Zone Standard Time");
    SetLastError(0xdead);
    ok = gtzify(2025, &bad, &t);
    DWORD err = GetLastError();
    printf("for year, unknown key: %d error %lu\n", ok, ok ? 0 : (unsigned long)err);
    return 0;
}
