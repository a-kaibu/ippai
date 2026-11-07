/* kernel32 time conversions (fixed inputs: nothing depends on the clock): SYSTEMTIME <->
 * FILETIME incl. leap days, the limits and invalid fields (which are checked, which ignored),
 * DOS date/time, CompareFileTime, explicit time zones (JST, a DST zone) through
 * SystemTimeToTzSpecificLocalTime / TzSpecificLocalTimeToSystemTime, and GetDateFormat /
 * GetTimeFormat pictures in the Japanese locale. */
#include "../probe.h"

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-64s = %lld err %lu\n", #expr, r_, r_ ? 0 : GetLastError()); } while (0)

static void pst(const char *l, const SYSTEMTIME *s) {
    printf("%s %u-%u-%u dow %u %u:%u:%u.%u\n", l, s->wYear, s->wMonth, s->wDay, s->wDayOfWeek, s->wHour, s->wMinute, s->wSecond, s->wMilliseconds);
}

static void st2ft(WORD y, WORD mo, WORD d, WORD h, WORD mi, WORD s, WORD ms, WORD dow) {
    SYSTEMTIME st = { y, mo, dow, d, h, mi, s, ms };
    FILETIME ft = { 0xdeadbeef, 0xdeadbeef };
    SetLastError(0);
    BOOL r = SystemTimeToFileTime(&st, &ft);
    printf("st2ft %u-%u-%u %u:%u:%u.%u dow %u: %d err %lu ft %08lx%08lx\n", y, mo, d, h, mi, s, ms, dow, r, r ? 0 : GetLastError(), ft.dwHighDateTime, ft.dwLowDateTime);
    if (r) {
        SYSTEMTIME back;
        FileTimeToSystemTime(&ft, &back);
        pst("  back", &back);
    }
}

static void ft2st(ULONGLONG v) {
    FILETIME ft = { (DWORD)v, (DWORD)(v >> 32) };
    SYSTEMTIME st;
    memset(&st, 0xcc, sizeof st);
    SetLastError(0);
    BOOL r = FileTimeToSystemTime(&ft, &st);
    printf("ft2st %016llx: %d err %lu\n", v, r, r ? 0 : GetLastError());
    if (r) pst("  ", &st);
}

static void dos(WORD date, WORD time) {
    FILETIME ft = { 0, 0 };
    BOOL r = DosDateTimeToFileTime(date, time, &ft);
    WORD d2 = 0xdead, t2 = 0xdead;
    BOOL r2 = r ? FileTimeToDosDateTime(&ft, &d2, &t2) : 0;
    printf("dos %04x %04x: %d ft %08lx%08lx back %d %04x %04x\n", date, time, r, ft.dwHighDateTime, ft.dwLowDateTime, r2, d2, t2);
}

static TIME_ZONE_INFORMATION zone(LONG bias, LONG dst) {
    TIME_ZONE_INFORMATION z;
    memset(&z, 0, sizeof z);
    z.Bias = bias;
    if (dst) {
        /* US rules: second Sunday of March 2:00 to first Sunday of November 2:00 */
        z.DaylightBias = dst;
        SYSTEMTIME a = { 0, 3, 0, 2, 2, 0, 0, 0 }, b = { 0, 11, 0, 1, 2, 0, 0, 0 };
        z.DaylightDate = a;
        z.StandardDate = b;
    }
    return z;
}

static void tz(TIME_ZONE_INFORMATION *z, WORD y, WORD mo, WORD d, WORD h, WORD mi) {
    SYSTEMTIME u = { y, mo, 0, d, h, mi, 0, 0 }, l, back;
    BOOL r = SystemTimeToTzSpecificLocalTime(z, &u, &l);
    printf("tz bias %ld utc %u-%u-%u %u:%u: %d", z->Bias, y, mo, d, h, mi, r);
    if (r) {
        printf(" -> %u-%u-%u %u:%u dow %u", l.wYear, l.wMonth, l.wDay, l.wHour, l.wMinute, l.wDayOfWeek);
        BOOL r2 = TzSpecificLocalTimeToSystemTime(z, &l, &back);
        printf(" back %d %u-%u-%u %u:%u", r2, back.wYear, back.wMonth, back.wDay, back.wHour, back.wMinute);
    }
    printf("\n");
}

int main(void) {
    st2ft(1601, 1, 1, 0, 0, 0, 0, 0);
    st2ft(1600, 12, 31, 23, 59, 59, 999, 0);
    st2ft(2000, 2, 29, 12, 0, 0, 0, 3);
    st2ft(1900, 2, 29, 0, 0, 0, 0, 0);
    st2ft(2100, 2, 29, 0, 0, 0, 0, 0);
    st2ft(2024, 2, 29, 23, 59, 59, 999, 6);
    st2ft(2024, 13, 1, 0, 0, 0, 0, 0);
    st2ft(2024, 0, 1, 0, 0, 0, 0, 0);
    st2ft(2024, 4, 31, 0, 0, 0, 0, 0);
    st2ft(2024, 4, 0, 0, 0, 0, 0, 0);
    st2ft(2024, 4, 30, 24, 0, 0, 0, 0);
    st2ft(2024, 4, 30, 0, 60, 0, 0, 0);
    st2ft(2024, 4, 30, 0, 0, 60, 0, 0);
    st2ft(2024, 4, 30, 0, 0, 0, 1000, 0);
    st2ft(2024, 4, 30, 0, 0, 0, 0, 9); /* day of week is ignored */
    st2ft(30827, 12, 31, 23, 59, 59, 999, 0);
    st2ft(30828, 1, 1, 0, 0, 0, 0, 0);
    st2ft(9999, 12, 31, 23, 59, 59, 999, 0);
    ft2st(0);
    ft2st(1);
    ft2st(9999999);
    ft2st(10000);
    ft2st(0x01d0000000000000ULL);
    ft2st(0x7fffffffffffffffULL);
    ft2st(0x8000000000000000ULL);
    ft2st(116444736000000000ULL); /* 1970 */
    ft2st(116444736000000000ULL + 0x7fffffffULL * 10000000ULL); /* 2038 */

    dos(0x0021, 0x0000); /* 1980-01-01 */
    dos(0x0000, 0x0000);
    dos(0x5a9e, 0xbf7d); /* 2025-04-30 23:59:58 */
    dos(0xff9f, 0xbf7d); /* 2107-12-31 */
    dos(0x5a9f, 0xc000); /* hour 24 */
    dos(0x5a9f, 0x003e); /* seconds 62 */
    dos(0x5a80, 0x0000); /* day 0 */
    dos(0x5abf, 0x0000); /* month 13 */
    FILETIME pre = { 0, 0 };
    WORD dd = 0xdead, tt = 0xdead;
    E(FileTimeToDosDateTime(&pre, &dd, &tt));
    printf("pre-1980 %04x %04x\n", dd, tt);

    FILETIME a = { 5, 1 }, b = { 6, 1 }, c = { 0xffffffff, 0 };
    E(CompareFileTime(&a, &b)); E(CompareFileTime(&b, &a)); E(CompareFileTime(&a, &a)); E(CompareFileTime(&c, &a));

    TIME_ZONE_INFORMATION jst = zone(-540, 0), us = zone(300, -60);
    tz(&jst, 2024, 12, 31, 20, 0);
    tz(&jst, 1601, 1, 1, 0, 0);
    tz(&us, 2024, 3, 10, 6, 59);
    tz(&us, 2024, 3, 10, 7, 0);
    tz(&us, 2024, 11, 3, 5, 59);
    tz(&us, 2024, 11, 3, 6, 0);
    tz(&us, 2024, 7, 1, 0, 0);
    tz(&us, 2024, 1, 1, 0, 0);

    /* date and time pictures (Japanese locale) */
    SYSTEMTIME st = { 2024, 3, 2, 5, 9, 7, 3, 4 };
    const char *dp[] = { NULL, "yyyy'/'MM'/'dd", "yy/M/d ddd dddd", "gg yyyy", "MMMM MMM", "'quote''s' d", "yyyyy" };
    for (int i = 0; i < 7; i++) {
        char out[128] = "";
        int n = GetDateFormatA(0x411, 0, &st, dp[i], out, sizeof out);
        printf("date [%s] -> %d [", dp[i] ? dp[i] : "(default)", n); for (char *p = out; *p; p++) printf("%02x", (unsigned char)*p); printf("]\n");
    }
    char out[128];
    E(GetDateFormatA(0x411, DATE_LONGDATE, &st, NULL, out, sizeof out)); for (char *p = out; *p; p++) printf("%02x", (unsigned char)*p); printf("\n");
    E(GetDateFormatA(0x411, 0, &st, "yyyy", out, 2));
    E(GetDateFormatA(0x411, 0, &st, "yyyy", NULL, 0));
    SYSTEMTIME bad = { 2024, 2, 0, 30, 0, 0, 0, 0 };
    E(GetDateFormatA(0x411, 0, &bad, NULL, out, sizeof out));
    const char *tp[] = { NULL, "HH':'mm':'ss", "h:m:s tt", "hh tt t", "H" };
    for (int i = 0; i < 5; i++) {
        out[0] = 0;
        int n = GetTimeFormatA(0x411, 0, &st, tp[i], out, sizeof out);
        printf("time [%s] -> %d [", tp[i] ? tp[i] : "(default)", n); for (char *p = out; *p; p++) printf("%02x", (unsigned char)*p); printf("]\n");
    }
    E(GetTimeFormatA(0x411, TIME_NOSECONDS, &st, NULL, out, sizeof out)); printf("%s\n", out);
    E(GetTimeFormatA(0x411, TIME_FORCE24HOURFORMAT | TIME_NOTIMEMARKER, &st, "h:mm tt", out, sizeof out)); printf("%s\n", out);
    SYSTEMTIME badt = { 2024, 1, 0, 1, 25, 0, 0, 0 };
    E(GetTimeFormatA(0x411, 0, &badt, NULL, out, sizeof out));
    return 0;
}
