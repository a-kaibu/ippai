/* ChangeDisplaySettings(Ex)(A/W) with CDS_TEST: Windows only answers whether the mode would
 * work, and the screen stays as it was. LiveMaker (s2011_waffle_sex_friend, #81) tests every
 * mode it lists with CDS_TEST; sake switched to each, ending at 1280x720, and a 1280x720 window
 * with its frame no longer fit. Only what holds on any machine is printed: the screen compared
 * with itself before, and a mode no display has. */
#include "../probe.h"

static int sw, sh;
static const char *same(void) {
    return GetSystemMetrics(SM_CXSCREEN) == sw && GetSystemMetrics(SM_CYSCREEN) == sh ? "same" : "changed";
}
static void modeA(DEVMODEA *d, DWORD w, DWORD h) {
    memset(d, 0, sizeof *d);
    d->dmSize = sizeof *d;
    d->dmPelsWidth = w;
    d->dmPelsHeight = h;
    d->dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
}
static void modeW(DEVMODEW *d, DWORD w, DWORD h) {
    memset(d, 0, sizeof *d);
    d->dmSize = sizeof *d;
    d->dmPelsWidth = w;
    d->dmPelsHeight = h;
    d->dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
}

int main(void) {
    sw = GetSystemMetrics(SM_CXSCREEN);
    sh = GetSystemMetrics(SM_CYSCREEN);
    DEVMODEA a;
    DEVMODEW w;
    /* (640x480: a mode every display lists; the current one; one no display has) */
    static const DWORD sizes[3][2] = { { 640, 480 }, { 0, 0 }, { 12345, 77 } };
    static const char *names[3] = { "640x480", "current", "12345x77" };
    for (int i = 0; i < 3; i++) {
        DWORD x = sizes[i][0] ? sizes[i][0] : (DWORD)sw, y = sizes[i][1] ? sizes[i][1] : (DWORD)sh;
        modeA(&a, x, y);
        LONG r = ChangeDisplaySettingsA(&a, CDS_TEST);
        printf("ChangeDisplaySettingsA %s test %ld screen %s\n", names[i], r, same());
        modeW(&w, x, y);
        r = ChangeDisplaySettingsW(&w, CDS_TEST);
        printf("ChangeDisplaySettingsW %s test %ld screen %s\n", names[i], r, same());
        modeA(&a, x, y);
        r = ChangeDisplaySettingsExA(NULL, &a, NULL, CDS_TEST, NULL);
        printf("ChangeDisplaySettingsExA %s test %ld screen %s\n", names[i], r, same());
        modeW(&w, x, y);
        r = ChangeDisplaySettingsExW(NULL, &w, NULL, CDS_TEST, NULL);
        printf("ChangeDisplaySettingsExW %s test %ld screen %s\n", names[i], r, same());
    }
    /* (a mode no display has, for real: refused, the screen as it was) */
    modeA(&a, 12345, 77);
    printf("ChangeDisplaySettingsA 12345x77 %ld screen %s\n", ChangeDisplaySettingsA(&a, 0), same());
    return 0;
}
