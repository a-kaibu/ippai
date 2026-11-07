/* What a hardware tool asks Windows about the machine besides CPUID (CPU-Z): ntdll's
 * NtQuerySystemInformation (basic, processor and per-processor time information, code integrity;
 * the size rules and status codes), the SMBIOS firmware table (its header, that its structures
 * walk to the end-of-table one), the installed memory and the power schemes. Only what holds on
 * any machine is printed (counts against GetSystemInfo, layouts, statuses).
 * libs: -lpowrprof */
#include "../probe.h"
#include <powrprof.h>

typedef LONG(WINAPI *query_fn)(ULONG, void *, ULONG, ULONG *);

int main(void) {
    query_fn q = (query_fn)GetProcAddress(GetModuleHandleW(L"ntdll.dll"), "NtQuerySystemInformation");
    printf("NtQuerySystemInformation found %d\n", q != NULL);
    SYSTEM_INFO si;
    GetSystemInfo(&si);
    BYTE buf[4096];
    ULONG ret;
    const ULONG basic = sizeof(void *) == 8 ? 64 : 44;
    for (ULONG len = basic - 1; len <= basic + 1; len++) {
        ret = 0xdead;
        memset(buf, 0, sizeof buf);
        LONG st = q(0, buf, len, &ret);
        printf("basic, length %lu: status %08lx return length %lu\n", len, (unsigned long)st, ret);
    }
    q(0, buf, basic, &ret);
    {
        ULONG page = *(ULONG *)(buf + 8), gran = *(ULONG *)(buf + 24);
        ULONG_PTR min = *(ULONG_PTR *)(buf + (sizeof(void *) == 8 ? 32 : 28)), max = *(ULONG_PTR *)(buf + (sizeof(void *) == 8 ? 40 : 32));
        ULONG_PTR mask = *(ULONG_PTR *)(buf + (sizeof(void *) == 8 ? 48 : 36));
        BYTE n = buf[sizeof(void *) == 8 ? 56 : 40];
        printf("basic: page %lu granularity %lu min %llx max==GetSystemInfo %d mask==GetSystemInfo %d processors==GetSystemInfo %d\n", page, gran,
               (unsigned long long)min, max == (ULONG_PTR)si.lpMaximumApplicationAddress, mask == si.dwActiveProcessorMask, n == si.dwNumberOfProcessors);
    }
    for (ULONG len = 11; len <= 13; len++) {
        ret = 0xdead;
        LONG st = q(1, buf, len, &ret);
        printf("processor, length %lu: status %08lx return length %lu\n", len, (unsigned long)st, ret);
    }
    q(1, buf, 12, &ret);
    printf("processor: architecture==GetSystemInfo %d level==GetSystemInfo %d revision==GetSystemInfo %d\n",
           *(USHORT *)buf == si.wProcessorArchitecture, *(USHORT *)(buf + 2) == si.wProcessorLevel, *(USHORT *)(buf + 4) == si.wProcessorRevision);
    {
        ULONG n = si.dwNumberOfProcessors, need = 48 * n;
        LONG st = q(8, buf, need - 1, &ret);
        printf("processor times, one byte short: status %08lx return length==48*processors %d\n", (unsigned long)st, ret == need);
        st = q(8, buf, need, &ret);
        printf("processor times: status %08lx return length==48*processors %d\n", (unsigned long)st, ret == need);
        st = q(8, buf, need + 48, &ret);
        printf("processor times, room for one more: status %08lx return length==48*processors %d\n", (unsigned long)st, ret == need);
        LONGLONG idle = *(LONGLONG *)buf, kernel = *(LONGLONG *)(buf + 8);
        printf("kernel time includes idle %d\n", kernel >= idle);
    }
    {
        ULONG ci[2] = { 8, 0xdead };
        LONG st = q(0x67, ci, sizeof ci, &ret);
        printf("code integrity: status %08lx return length %lu length field %lu\n", (unsigned long)st, ret, ci[0]);
        ci[0] = 4;
        st = q(0x67, ci, 4, &ret);
        printf("code integrity, 4 bytes: status %08lx\n", (unsigned long)st);
    }
    printf("class 0x7fff: status %08lx\n", (unsigned long)q(0x7fff, buf, sizeof buf, &ret));

    /* firmware tables */
    const DWORD rsmb = 'RSMB';
    SetLastError(0xdead);
    UINT need = GetSystemFirmwareTable(rsmb, 0, NULL, 0);
    printf("RSMB size query: nonzero %d\n", need > 8);
    SetLastError(0xdead);
    UINT small = GetSystemFirmwareTable(rsmb, 0, buf, 4);
    printf("RSMB into 4 bytes: returns the size %d\n", small == need);
    BYTE *t = malloc(need);
    UINT got = GetSystemFirmwareTable(rsmb, 0, t, need);
    printf("RSMB: got==size %d calling method %u major>=2 %d length==size-8 %d\n", got == need, t[0], t[1] >= 2, *(DWORD *)(t + 4) == need - 8);
    {
        BYTE *p = t + 8, *end = t + need;
        int has0 = 0, has1 = 0, has2 = 0, ended = 0, n = 0;
        while (p + 4 <= end && !ended) {
            if (p[0] == 0) has0 = 1;
            if (p[0] == 1) has1 = 1;
            if (p[0] == 2) has2 = 1;
            if (p[0] == 127) ended = 1;
            BYTE *s = p + p[1];
            while (s + 1 < end && (s[0] || s[1])) s++;
            p = s + 2;
            n++;
        }
        printf("RSMB walk: BIOS %d system %d baseboard %d end-of-table %d within %d\n", has0, has1, has2, ended, p <= end);
    }
    free(t);
    SetLastError(0xdead);
    UINT t1 = GetSystemFirmwareTable(rsmb, 1, buf, sizeof buf);
    printf("RSMB table 1: the same size %d err %lu\n", t1 == need, GetLastError());
    DWORD ids[8] = { 0xdead };
    UINT en = EnumSystemFirmwareTables(rsmb, ids, sizeof ids);
    printf("EnumSystemFirmwareTables RSMB: %u first %lu\n", en, ids[0]);

    ULONGLONG kb = 0;
    BOOL ok = GetPhysicallyInstalledSystemMemory(&kb);
    printf("GetPhysicallyInstalledSystemMemory %d nonzero %d\n", ok, kb > 0);
    SetLastError(0xdead);
    ok = GetPhysicallyInstalledSystemMemory(NULL);
    printf("GetPhysicallyInstalledSystemMemory(NULL) %d err %lu\n", ok, GetLastError());

    /* power schemes: the three Windows has, one of them active, with names */
    GUID *active = NULL;
    printf("PowerGetActiveScheme %lu\n", PowerGetActiveScheme(NULL, &active));
    int found = 0, count = 0;
    for (ULONG i = 0; i < 16; i++) {
        GUID g;
        DWORD size = sizeof g;
        DWORD r = PowerEnumerate(NULL, NULL, NULL, ACCESS_SCHEME, i, (UCHAR *)&g, &size);
        if (r) {
            printf("PowerEnumerate %lu -> %lu\n", i, r);
            break;
        }
        count++;
        if (active && IsEqualGUID(&g, active)) found = 1;
        DWORD nl = 0;
        DWORD rn = PowerReadFriendlyName(NULL, &g, NULL, NULL, NULL, &nl);
        printf("scheme %lu: name size query %lu nonempty %d\n", i, rn, nl > 2);
    }
    printf("schemes %d, the active one among them %d\n", count, found);
    {
        DWORD size = 0;
        DWORD r = PowerEnumerate(NULL, NULL, NULL, ACCESS_SCHEME, 0, NULL, &size);
        printf("PowerEnumerate size query %lu size %lu\n", r, size);
        size = 4;
        GUID g;
        r = PowerEnumerate(NULL, NULL, NULL, ACCESS_SCHEME, 0, (UCHAR *)&g, &size);
        printf("PowerEnumerate small %lu size %lu\n", r, size);
    }
    LocalFree(active);
    return 0;
}
