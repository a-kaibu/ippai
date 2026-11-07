/* Structures whose size the caller gives (cb, cbSize, dmSize, dwLength, uiParam, the size
 * argument): what Windows writes for a short, an exact, a zero and an oversized one, and when it
 * refuses. sake once wrote the whole current layout over a game's short struct on its stack
 * (EnumDisplayDevices: YU-RIS, 56b52a8; EnumDisplaySettings: RealLive, e170d29), zeroing its
 * return address. Each call starts on a buffer of 0xcd bytes; "beyond" says whether anything
 * past the caller's size changed, "size" is the size field afterwards. Only what holds on any
 * machine is printed (no device names, no screen sizes but compared with GetSystemMetrics).
 * libs: -lwinmm */
#include "../probe.h"
#include <mmsystem.h>

static unsigned char buf[4096];
static void prep(unsigned size_at, unsigned size, int width) {
    memset(buf, 0xcd, sizeof buf);
    if (width == 2) *(WORD *)(buf + size_at) = (WORD)size;
    else if (width == 4) *(DWORD *)(buf + size_at) = size;
    SetLastError(0xdead);
}
/* changed bytes at or past `from` */
static int beyond(unsigned from) {
    for (unsigned i = from; i < sizeof buf; i++)
        if (buf[i] != 0xcd) return 1;
    return 0;
}
static int written(unsigned from, unsigned to) {
    for (unsigned i = from; i < to && i < sizeof buf; i++)
        if (buf[i] != 0xcd) return 1;
    return 0;
}
static unsigned long err(void) { return GetLastError(); }

static void devmode(int wide) {
    /* DEVMODEA: dmSize at 36, 124 bytes in Windows 95's layout, 156 now; W: at 68, 188/220 */
    unsigned at = wide ? 68 : 36, d = wide ? 32 : 0, e = wide ? 64 : 0;
    unsigned sizes[] = { 0, 1, 60 + d, 124 + e, 148 + e, 156 + e, 300 };
    for (int i = 0; i < 7; i++) {
        unsigned s = sizes[i];
        prep(at, s, 2);
        *(WORD *)(buf + at + 2) = 0; /* dmDriverExtra */
        BOOL r = wide ? EnumDisplaySettingsW(NULL, ENUM_CURRENT_SETTINGS, (DEVMODEW *)buf) : EnumDisplaySettingsA(NULL, ENUM_CURRENT_SETTINGS, (DEVMODEA *)buf);
        unsigned long er = err();
        unsigned got = *(WORD *)(buf + at);
        unsigned end = s < at + 4 ? at + 4 : s;
        int sized = r && got >= 124 + e;
        printf("EnumDisplaySettings%c current dmSize %u: ret %d err %lu size %u name %d beyond %d", wide ? 'W' : 'A', s, r != 0, r ? 0 : er, got,
               written(0, at), beyond(end));
        if (sized) {
            DWORD w = *(DWORD *)(buf + 108 + e), h = *(DWORD *)(buf + 112 + e), bpp = *(DWORD *)(buf + 104 + e);
            printf(" bpp>0 %d screen %d", bpp > 0, w == (DWORD)GetSystemMetrics(SM_CXSCREEN) && h == (DWORD)GetSystemMetrics(SM_CYSCREEN));
        }
        printf("\n");
    }
    /* a mode past the last one; a mode index 0 with the old size */
    prep(at, 124 + e, 2);
    BOOL r = wide ? EnumDisplaySettingsW(NULL, 100000, (DEVMODEW *)buf) : EnumDisplaySettingsA(NULL, 100000, (DEVMODEA *)buf);
    printf("EnumDisplaySettings%c mode 100000: ret %d beyond %d\n", wide ? 'W' : 'A', r != 0, beyond(124 + e));
    prep(at, 124 + e, 2);
    r = wide ? EnumDisplaySettingsW(NULL, 0, (DEVMODEW *)buf) : EnumDisplaySettingsA(NULL, 0, (DEVMODEA *)buf);
    printf("EnumDisplaySettings%c mode 0 dmSize old: ret %d size %u beyond %d\n", wide ? 'W' : 'A', r != 0, *(WORD *)(buf + at), beyond(124 + e));
}

static void display_device(int wide) {
    /* DISPLAY_DEVICEA: cb, DeviceName[32], DeviceString[128], StateFlags, DeviceID[128],
     * DeviceKey[128] = 424; W 840 */
    unsigned u = wide ? 2 : 1, full = 4 + (32 + 128) * u + 4 + 256 * u;
    unsigned flags_at = 4 + 160 * u;
    unsigned sizes[] = { 0, 3, 4, 4 + 32 * u, flags_at, flags_at + 4, full - 1, full, full + 100 };
    for (int i = 0; i < 9; i++) {
        unsigned s = sizes[i];
        prep(0, s, 4);
        BOOL r = wide ? EnumDisplayDevicesW(NULL, 0, (DISPLAY_DEVICEW *)buf, 0) : EnumDisplayDevicesA(NULL, 0, (DISPLAY_DEVICEA *)buf, 0);
        unsigned long er = err();
        printf("EnumDisplayDevices%c adapter 0 cb %u: ret %d err %lu cb %lu beyond %d", wide ? 'W' : 'A', s, r != 0, r ? 0 : er, *(DWORD *)buf,
               beyond(s < 4 ? 4 : s));
        if (r && s >= flags_at + 4) printf(" primary %d", (*(DWORD *)(buf + flags_at) & DISPLAY_DEVICE_PRIMARY_DEVICE) != 0);
        if (r && s >= 4 + 32 * u) printf(" name terminated %d", wide ? wcsnlen((WCHAR *)(buf + 4), 32) < 32 : strnlen((char *)buf + 4, 32) < 32);
        printf("\n");
    }
    prep(0, full, 4);
    BOOL r = wide ? EnumDisplayDevicesW(NULL, 1000, (DISPLAY_DEVICEW *)buf, 0) : EnumDisplayDevicesA(NULL, 1000, (DISPLAY_DEVICEA *)buf, 0);
    printf("EnumDisplayDevices%c adapter 1000: ret %d beyond %d\n", wide ? 'W' : 'A', r != 0, beyond(4));
}

static void monitor_info(int wide) {
    HMONITOR m = MonitorFromPoint((POINT){ 0, 0 }, MONITOR_DEFAULTTOPRIMARY);
    unsigned ex = wide ? 104 : 72;
    unsigned sizes[] = { 0, 39, 40, 41, 71, 72, 104, 200 };
    for (int i = 0; i < 8; i++) {
        unsigned s = sizes[i];
        prep(0, s, 4);
        BOOL r = wide ? GetMonitorInfoW(m, (MONITORINFO *)buf) : GetMonitorInfoA(m, (MONITORINFO *)buf);
        unsigned long er = err();
        printf("GetMonitorInfo%c cbSize %u: ret %d err %lu cbSize %lu beyond %d", wide ? 'W' : 'A', s, r != 0, r ? 0 : er, *(DWORD *)buf, beyond(s < 4 ? 4 : s));
        if (r) {
            RECT *rc = (RECT *)(buf + 4);
            printf(" monitor %d primary %lu", rc->left == 0 && rc->top == 0 && rc->right == GetSystemMetrics(SM_CXSCREEN) && rc->bottom == GetSystemMetrics(SM_CYSCREEN),
                   *(DWORD *)(buf + 36));
            if (s >= ex) {
                if (wide) printf(" device %d", wcsncmp((WCHAR *)(buf + 40), L"\\\\.\\DISPLAY", 11) == 0);
                else printf(" device %d", strncmp((char *)buf + 40, "\\\\.\\DISPLAY", 11) == 0);
            }
        }
        printf("\n");
    }
}

static void nonclient(int wide) {
    /* NONCLIENTMETRICSA: 340 bytes before Vista, 344 with iPaddedBorderWidth; W 500/504 */
    unsigned old = wide ? 500 : 340;
    unsigned sizes[] = { 0, old - 4, old, old + 4, old + 8, 1000 };
    for (int i = 0; i < 6; i++) {
        for (int same = 1; same >= 0; same--) {
            unsigned s = sizes[i];
            prep(0, s, 4);
            UINT ui = same ? s : old + 4;
            BOOL r = wide ? SystemParametersInfoW(SPI_GETNONCLIENTMETRICS, ui, buf, 0) : SystemParametersInfoA(SPI_GETNONCLIENTMETRICS, ui, buf, 0);
            unsigned long er = err();
            printf("SystemParametersInfo%c NONCLIENTMETRICS cbSize %u uiParam %u: ret %d err %lu cbSize %lu beyond %d", wide ? 'W' : 'A', s, ui, r != 0, r ? 0 : er,
                   *(DWORD *)buf, beyond(s < 4 ? 4 : s));
            if (r) {
                int nz = 1;
                for (int k = 1; k <= 5; k++) nz &= ((int *)buf)[k] > 0;
                unsigned lf = wide ? 92 : 60;
                /* iSmCaptionWidth/Height after lfCaptionFont, iMenuWidth/Height after lfSmCaptionFont */
                int sm = *(int *)(buf + 24 + lf) > 0 && *(int *)(buf + 28 + lf) > 0;
                int menu = *(int *)(buf + 32 + 2 * lf) > 0 && *(int *)(buf + 36 + 2 * lf) > 0;
                /* each LOGFONT's face name is set */
                int faces = 1;
                unsigned fonts[5] = { 24, 32 + lf, 40 + 2 * lf, 40 + 3 * lf, 40 + 4 * lf };
                for (int k = 0; k < 5; k++) faces &= buf[fonts[k] + 28] != 0 && buf[fonts[k] + 28] != 0xcd;
                printf(" metrics %d small caption %d menu %d faces %d", nz, sm, menu, faces);
            }
            printf("\n");
        }
    }
}

static void version_ex(int wide) {
    unsigned base = wide ? 276 : 148, ex = wide ? 284 : 156;
    unsigned sizes[] = { 0, base - 1, base, base + 1, ex - 1, ex, ex + 100 };
    for (int i = 0; i < 7; i++) {
        unsigned s = sizes[i];
        prep(0, s, 4);
        BOOL r = wide ? GetVersionExW((OSVERSIONINFOW *)buf) : GetVersionExA((OSVERSIONINFOA *)buf);
        unsigned long er = err();
        printf("GetVersionEx%c size %u: ret %d err %lu size %lu beyond %d", wide ? 'W' : 'A', s, r != 0, r ? 0 : er, *(DWORD *)buf, beyond(s < 4 ? 4 : s));
        if (r) printf(" major>=5 %d platform %lu", *(DWORD *)(buf + 4) >= 5, *(DWORD *)(buf + 16));
        printf("\n");
    }
}

static void memory_status(void) {
    unsigned sizes[] = { 0, 32, 63, 64, 65, 128 };
    for (int i = 0; i < 6; i++) {
        unsigned s = sizes[i];
        prep(0, s, 4);
        BOOL r = GlobalMemoryStatusEx((MEMORYSTATUSEX *)buf);
        unsigned long er = err();
        printf("GlobalMemoryStatusEx dwLength %u: ret %d err %lu length %lu beyond %d", s, r != 0, r ? 0 : er, *(DWORD *)buf, beyond(s < 4 ? 4 : s));
        if (r) printf(" load<=100 %d total>0 %d", *(DWORD *)(buf + 4) <= 100, *(ULONGLONG *)(buf + 8) > 0);
        printf("\n");
    }
    unsigned old[] = { 0, 32, 100 };
    for (int i = 0; i < 3; i++) {
        prep(0, old[i], 4);
        GlobalMemoryStatus((MEMORYSTATUS *)buf);
        printf("GlobalMemoryStatus dwLength %u: length %lu beyond %d\n", old[i], *(DWORD *)buf, beyond(sizeof(MEMORYSTATUS)));
    }
}

static void scroll_info(void) {
    HWND w = CreateWindowExA(0, "STATIC", "", WS_POPUP | WS_VSCROLL | WS_HSCROLL, 0, 0, 200, 200, NULL, NULL, NULL, NULL);
    SCROLLINFO si = { sizeof si, SIF_RANGE | SIF_PAGE | SIF_POS, 0, 100, 10, 30, 0 };
    int pos = SetScrollInfo(w, SB_VERT, &si, FALSE);
    printf("SetScrollInfo vert: %d\n", pos);
    si.cbSize = 0;
    SetLastError(0xdead);
    pos = SetScrollInfo(w, SB_HORZ, &si, FALSE);
    printf("SetScrollInfo horz cbSize 0: %d err %lu\n", pos, GetLastError());
    unsigned sizes[] = { 0, 24, 27, 28, 29, 100 };
    for (int i = 0; i < 6; i++) {
        unsigned s = sizes[i];
        prep(0, s, 4);
        *(DWORD *)(buf + 4) = SIF_ALL;
        BOOL r = GetScrollInfo(w, SB_VERT, (SCROLLINFO *)buf);
        unsigned long er = err();
        printf("GetScrollInfo cbSize %u: ret %d err %lu beyond %d", s, r != 0, r ? 0 : er, beyond(s < 8 ? 8 : s));
        if (r) printf(" min %d max %d page %u pos %d", *(int *)(buf + 8), *(int *)(buf + 12), *(UINT *)(buf + 16), *(int *)(buf + 20));
        printf("\n");
    }
    /* only the asked fields */
    prep(0, 28, 4);
    *(DWORD *)(buf + 4) = SIF_POS;
    BOOL r = GetScrollInfo(w, SB_VERT, (SCROLLINFO *)buf);
    printf("GetScrollInfo SIF_POS: ret %d range written %d page written %d pos %d\n", r != 0, written(8, 16), written(16, 20), *(int *)(buf + 20));
    printf("GetScrollPos vert %d horz %d\n", GetScrollPos(w, SB_VERT), GetScrollPos(w, SB_HORZ));
    int mn = -1, mx = -1;
    GetScrollRange(w, SB_VERT, &mn, &mx);
    printf("GetScrollRange vert %d %d\n", mn, mx);
    int was = SetScrollPos(w, SB_VERT, 50, FALSE);
    printf("SetScrollPos vert 50 -> %d now %d\n", was, GetScrollPos(w, SB_VERT));
    /* past the end: clamped to max - page + 1 */
    was = SetScrollPos(w, SB_VERT, 500, FALSE);
    printf("SetScrollPos vert 500 -> %d now %d\n", was, GetScrollPos(w, SB_VERT));
    DestroyWindow(w);
}

static void time_caps(void) {
    unsigned sizes[] = { 0, 4, 7, 8, 16 };
    for (int i = 0; i < 5; i++) {
        unsigned s = sizes[i];
        memset(buf, 0xcd, sizeof buf);
        MMRESULT r = timeGetDevCaps((TIMECAPS *)buf, s);
        printf("timeGetDevCaps size %u: ret %u beyond %d", s, r, beyond(s));
        if (r == 0) printf(" min %u max>=1000000 %d", *(UINT *)buf, *(UINT *)(buf + 4) >= 1000000);
        printf("\n");
    }
}

static void startup_info(void) {
    prep(0, 0, 4);
    GetStartupInfoA((STARTUPINFOA *)buf);
    printf("GetStartupInfoA: cb %lu beyond %d\n", *(DWORD *)buf, beyond(sizeof(STARTUPINFOA)));
    prep(0, 0, 4);
    GetStartupInfoW((STARTUPINFOW *)buf);
    printf("GetStartupInfoW: cb %lu beyond %d\n", *(DWORD *)buf, beyond(sizeof(STARTUPINFOW)));
}

int main(void) {
    devmode(0);
    devmode(1);
    display_device(0);
    display_device(1);
    monitor_info(0);
    monitor_info(1);
    nonclient(0);
    nonclient(1);
    version_ex(0);
    version_ex(1);
    memory_status();
    scroll_info();
    time_caps();
    startup_info();
    return 0;
}
